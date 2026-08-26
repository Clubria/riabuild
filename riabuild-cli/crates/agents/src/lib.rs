//! `riabuild agents` — Claude Code, Codex and Grok Build in one window.
//!
//! The three harnesses are driven **headless**: each runs in its own structured
//! output mode and this crate draws the result, rather than embedding each
//! vendor's own full-screen interface in a pane. That choice is the whole
//! architecture, and it buys the one thing a terminal multiplexer cannot give —
//! *state*. Screen-scraping three alternate-screen TUIs tells you what pixels
//! changed; reading their event streams tells you which agent is blocked, what
//! it is running, and what it has spent.
//!
//! # A turn outlives the window that started it
//!
//! Nothing here owns a running agent. A turn is `riabuild internal agent-turn`,
//! started detached, holding the session's lock and appending the harness's
//! stdout to a spool file. This window *reads* that file — while the turn runs,
//! and again tomorrow when it is reopened. So closing it interrupts nothing,
//! reopening it shows everything that happened in between, and a reboot loses
//! only the process, never the conversation.
//!
//! That is also why there is no fleet and no child handle anywhere in this
//! crate. Owning a child would mean the turn ends when the window does, which is
//! precisely what this design exists to avoid.
//!
//! # Ownership of the terminal
//!
//! This is the third thing in riabuild that writes to a terminal, and it is a
//! different thing from the other two. `riabuild-ui` prints lines *past* a
//! terminal it does not own; `run_interactive` hands the terminal to a child and
//! looks away. This takes the terminal — raw mode, alternate screen — draws
//! whole frames, and gives it back. It is confined to this crate: nothing here
//! prints, and nothing outside here draws.
//!
//! The async-IO invariant survives intact. Keys are read on a dedicated OS
//! thread rather than on the reactor, for the reason `runner/pty.rs` uses
//! `AsyncFd`: a blocking `read()` on the current-thread runtime would hold every
//! session's output behind whether a developer happens to be typing.

// `unwrap_used`, `panic` and `expect_used` are denied workspace-wide. In a test
// a panic *is* how a failed precondition is reported, so unwrapping a fixture is
// correct — but the exemption is `test` and nothing else. Spelling it
// `any(test, feature = "…")` would switch the lint off for this crate's
// production code under the one command that enforces it, because
// `cargo clippy --workspace --all-targets` resolves dev-dependencies and
// features unify onto the lib target. See `riabuild-theme`, where that bug was
// found and the reasoning is written out in full.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{event, execute};
use riabuild_harness::{Kind, Reader};
use riabuild_runner::CommandRunner;
use riabuild_theme::Theme;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

pub mod app;
pub mod draw;
pub mod store;
pub mod turn;

use app::{App, Focus, Pane};
use store::Store;

/// Which sign-in each harness runs under.
///
/// Resolved by the caller from riabuild's own account list, and recorded on the
/// session when it is created — never recomputed. A session is only resumable
/// under the profile that made it, so if the primary Claude account changes
/// between turns, a recomputed home would point at a different store and the
/// conversation would silently start over as a new one.
#[derive(Debug, Clone, Default)]
pub struct Homes {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
    pub grok: Option<PathBuf>,
}

impl Homes {
    fn get(&self, kind: Kind) -> Option<PathBuf> {
        match kind {
            Kind::Claude => self.claude.clone(),
            Kind::Codex => self.codex.clone(),
            Kind::Grok => self.grok.clone(),
        }
    }
}

/// What `riabuild agents` was asked to do.
#[derive(Debug, Clone)]
pub struct Request {
    /// This riabuild, by absolute path — what a turn is started through.
    ///
    /// From `shims::running_binary`, for the reason every generated shim names
    /// it in full: riabuild is the one tool riabuild does not put on `PATH`, so
    /// a bare name finds another machine's copy or nothing at all.
    pub riabuild: PathBuf,
    /// The checkout every session in this window belongs to.
    pub cwd: PathBuf,
    pub homes: Homes,
    /// The first thing to say, asked of every harness at once.
    pub prompt: Option<String>,
    pub theme: Theme,
    /// Whether this terminal can be trusted with the block glyphs, which is
    /// `riabuild-ui`'s decision rather than one made again here.
    pub unicode: bool,
}

/// What a keypress asks for.
///
/// Returned rather than performed, so the whole keymap is testable without a
/// terminal, a process or a filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Quit,
    Send(String),
    Open(Kind),
}

/// The keymap.
pub fn key(app: &mut App, event: KeyEvent) -> Action {
    // Windows sends both press and release; without this every key acts twice.
    // Harmless on the two platforms riabuild supports and wrong to leave out.
    if event.kind == KeyEventKind::Release {
        return Action::Nothing;
    }
    // Ctrl-C leaves, from either mode. A developer who has just typed half a
    // prompt still expects it to work — and leaving now interrupts nothing,
    // because the turn is not this process's child.
    if event.modifiers.contains(KeyModifiers::CONTROL) && event.code == KeyCode::Char('c') {
        return Action::Quit;
    }

    match app.focus {
        Focus::Compose => match event.code {
            KeyCode::Esc => {
                app.focus = Focus::List;
                Action::Nothing
            }
            KeyCode::Enter => {
                let text = app.composing.trim().to_string();
                app.composing.clear();
                app.focus = Focus::List;
                if text.is_empty() {
                    Action::Nothing
                } else {
                    Action::Send(text)
                }
            }
            KeyCode::Backspace => {
                app.composing.pop();
                Action::Nothing
            }
            KeyCode::Char(ch) => {
                app.composing.push(ch);
                Action::Nothing
            }
            _ => Action::Nothing,
        },
        Focus::List => match event.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                app.select_next();
                Action::Nothing
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                app.select_previous();
                Action::Nothing
            }
            KeyCode::PageUp => {
                app.scrollback = app.scrollback.saturating_add(10);
                Action::Nothing
            }
            KeyCode::PageDown => {
                app.scrollback = app.scrollback.saturating_sub(10);
                Action::Nothing
            }
            KeyCode::Char('n') | KeyCode::Char('1') => Action::Open(Kind::Claude),
            KeyCode::Char('2') => Action::Open(Kind::Codex),
            KeyCode::Char('3') => Action::Open(Kind::Grok),
            KeyCode::Enter => {
                if app.selected().is_some() {
                    app.focus = Focus::Compose;
                }
                Action::Nothing
            }
            _ => Action::Nothing,
        },
    }
}

/// Reads keys on a thread of their own.
///
/// A dedicated OS thread rather than a task: `event::read` blocks, and blocking
/// the current-thread runtime would stall every session's output for as long as
/// nobody is typing — which is most of the time. The thread is detached and ends
/// with the process; there is nothing to join, because it is parked inside a
/// `read` that only the terminal can complete.
fn keys() -> UnboundedReceiver<TermEvent> {
    let (tx, rx) = unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            if tx.send(event).is_err() {
                break;
            }
        }
    });
    rx
}

/// Takes the terminal, runs until the developer leaves, and gives it back.
pub async fn run(
    runner: Arc<dyn CommandRunner>,
    paths: &dyn riabuild_paths::Paths,
    request: Request,
) -> Result<()> {
    let store = Store::new(paths);
    // Before anything is listed, so the cap is enforced by using the window
    // rather than by a command nobody remembers to run.
    let _ = store.prune(&request.cwd).await;

    let mut app = App::new();
    let mut readers: HashMap<String, Reader> = HashMap::new();
    restore(&store, &request, &mut app, &mut readers).await?;

    if let Some(prompt) = request.prompt.clone() {
        for index in 0..app.panes.len() {
            app.selected = index;
            send(&store, runner.as_ref(), &request, &mut app, &prompt).await;
        }
        app.selected = 0;
    }

    let mut terminal = claim().context("could not take the terminal")?;
    // Whatever happens below, the terminal is handed back. A provisioner that
    // left a developer in raw mode on the alternate screen would be worse than
    // one that simply failed.
    let outcome = drive(
        &mut terminal,
        &store,
        runner.as_ref(),
        &request,
        &mut app,
        &mut readers,
    )
    .await;
    release(&mut terminal);
    outcome
}

/// Loads this checkout's sessions and replays what they have already said.
async fn restore(
    store: &Store,
    request: &Request,
    app: &mut App,
    readers: &mut HashMap<String, Reader>,
) -> Result<()> {
    for record in store.sessions(&request.cwd).await.unwrap_or_default() {
        let Some(kind) = record.harness() else {
            continue;
        };
        let mut pane = Pane::new(record.id.clone(), kind, record.title.clone());
        pane.thread = record.thread.clone();
        // Replayed through the same decoder a live turn is read with, so a
        // reopened pane shows what was on screen when the work happened rather
        // than a reconstruction of it.
        let mut reader = Reader::new(kind);
        let spool = store.spool(&record.id).await.unwrap_or_default();
        pane.offset = spool.len() as u64;
        for line in spool.lines() {
            for event in reader.read(line) {
                pane.observe(&event);
            }
        }
        // Replayed like the spool, so a failure from yesterday's turn is still
        // on screen when the window comes back.
        let (trouble, at) = store.trouble_since(&record.id, 0).await.unwrap_or_default();
        for line in trouble.lines().filter(|line| !line.trim().is_empty()) {
            pane.observe(&riabuild_harness::Event::Trouble(line.to_string()));
        }
        pane.trouble_offset = at;
        pane.running = store.running(&record.id).await;
        readers.insert(record.id.clone(), reader);
        app.add(pane);
    }

    // Every harness riabuild installs gets a pane. One that already has a
    // session keeps it — reopening should hand a developer back the conversation
    // they were having, not a fourth empty pane beside it.
    for kind in Kind::ALL {
        if app.panes.iter().any(|pane| pane.kind == kind) {
            continue;
        }
        let record = store
            .create(kind, &request.cwd, request.homes.get(kind))
            .await?;
        readers.insert(record.id.clone(), Reader::new(kind));
        app.add(Pane::new(record.id, kind, String::new()));
    }
    app.selected = 0;
    Ok(())
}

type Screen = Terminal<CrosstermBackend<std::io::Stdout>>;

fn claim() -> Result<Screen> {
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn release(terminal: &mut Screen) {
    // Every step is attempted even if an earlier one failed: leaving the
    // alternate screen matters more than reporting why raw mode would not come
    // off, and there is nobody to report to until it has.
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = terminal.show_cursor();
}

async fn drive(
    terminal: &mut Screen,
    store: &Store,
    runner: &dyn CommandRunner,
    request: &Request,
    app: &mut App,
    readers: &mut HashMap<String, Reader>,
) -> Result<()> {
    let mut keys = keys();
    // Fast enough for the spinner to read as motion and for output to feel live,
    // slow enough that an idle window is not reading three files a hundred times
    // a second.
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(120));

    loop {
        terminal.draw(|frame| draw::render(frame, app, request.theme, request.unicode))?;
        if app.quit {
            return Ok(());
        }

        let action = tokio::select! {
            Some(event) = keys.recv() => match event {
                TermEvent::Key(pressed) => key(app, pressed),
                _ => Action::Nothing,
            },
            _ = ticker.tick() => {
                app.tick = app.tick.wrapping_add(1);
                follow(store, app, readers).await;
                Action::Nothing
            }
        };

        match action {
            Action::Nothing => {}
            Action::Quit => app.quit = true,
            Action::Open(kind) => {
                let record = store
                    .create(kind, &request.cwd, request.homes.get(kind))
                    .await?;
                readers.insert(record.id.clone(), Reader::new(kind));
                app.add(Pane::new(record.id, kind, String::new()));
                app.selected = app.panes.len().saturating_sub(1);
            }
            Action::Send(text) => send(store, runner, request, app, &text).await,
        }
    }
}

/// Reads whatever the running turns have appended since the last tick.
async fn follow(store: &Store, app: &mut App, readers: &mut HashMap<String, Reader>) {
    let ids: Vec<(String, u64, u64)> = app
        .panes
        .iter()
        .map(|pane| (pane.id.clone(), pane.offset, pane.trouble_offset))
        .collect();
    for (id, offset, trouble_at) in ids {
        if let Ok((fresh, moved)) = store.spool_since(&id, offset).await
            && !fresh.is_empty()
        {
            if let Some(reader) = readers.get_mut(&id) {
                let events: Vec<_> = fresh.lines().flat_map(|line| reader.read(line)).collect();
                for event in events {
                    app.observe(&id, &event);
                }
            }
            if let Some(pane) = app.pane_mut(&id) {
                pane.offset = moved;
            }
        }
        // riabuild's own failures, which have nowhere in the harness's stream to
        // live: a binary that would not start writes here and nowhere else.
        if let Ok((trouble, at)) = store.trouble_since(&id, trouble_at).await
            && !trouble.is_empty()
        {
            for line in trouble.lines().filter(|line| !line.trim().is_empty()) {
                app.observe(&id, &riabuild_harness::Event::Trouble(line.to_string()));
            }
            if let Some(pane) = app.pane_mut(&id) {
                pane.trouble_offset = at;
            }
        }
        // Asked every tick rather than inferred from the stream: a turn can also
        // end by being killed, and nothing is written then.
        let running = store.running(&id).await;
        app.set_running(&id, running);
    }
}

/// Starts a turn for the selected session.
async fn send(
    store: &Store,
    runner: &dyn CommandRunner,
    request: &Request,
    app: &mut App,
    text: &str,
) {
    let Some(id) = app.selected().map(|pane| pane.id.clone()) else {
        return;
    };
    app.sent(text);

    // Re-read rather than trusting what this window remembers: a turn started
    // from another window may have learned the thread id since, and resuming
    // without it starts a second conversation instead of continuing this one.
    let record = match store.read(&id).await {
        Ok(mut record) => {
            if record.title.is_empty() {
                record.title = store::title_of(text);
                let _ = store.write(&record).await;
            }
            record
        }
        Err(error) => {
            app.observe(&id, &riabuild_harness::Event::Trouble(format!("{error:#}")));
            app.set_running(&id, false);
            return;
        }
    };

    // A turn that will not start is this session's problem and not the window's:
    // the other agents keep running and the pane says what happened.
    if let Err(error) = store
        .start_turn(runner, &request.riabuild, &record, text)
        .await
    {
        app.observe(&id, &riabuild_harness::Event::Trouble(format!("{error:#}")));
        app.set_running(&id, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn with_one_session() -> App {
        let mut app = App::new();
        app.add(Pane::new("s1".into(), Kind::Claude, String::new()));
        app
    }

    #[test]
    fn typing_a_prompt_and_pressing_enter_sends_it() {
        let mut app = with_one_session();
        assert_eq!(key(&mut app, press(KeyCode::Enter)), Action::Nothing);
        assert_eq!(app.focus, Focus::Compose);
        for ch in "hello".chars() {
            key(&mut app, press(KeyCode::Char(ch)));
        }
        assert_eq!(app.composing, "hello");
        assert_eq!(
            key(&mut app, press(KeyCode::Enter)),
            Action::Send("hello".into())
        );
        // and the box is cleared, so the next prompt does not start with the last
        assert_eq!(app.composing, "");
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn an_empty_prompt_sends_nothing() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        key(&mut app, press(KeyCode::Char(' ')));
        assert_eq!(key(&mut app, press(KeyCode::Enter)), Action::Nothing);
    }

    #[test]
    fn letters_are_letters_while_typing_and_commands_otherwise() {
        // The bug this stops: `q` typed into a prompt quitting the program, and
        // taking the half-written message with it.
        let mut app = with_one_session();
        assert_eq!(key(&mut app, press(KeyCode::Char('q'))), Action::Quit);

        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        assert_eq!(key(&mut app, press(KeyCode::Char('q'))), Action::Nothing);
        assert_eq!(app.composing, "q");
        assert_eq!(key(&mut app, press(KeyCode::Char('n'))), Action::Nothing);
        assert_eq!(app.composing, "qn");
    }

    #[test]
    fn ctrl_c_leaves_from_either_mode() {
        for focus in [Focus::List, Focus::Compose] {
            let mut app = with_one_session();
            app.focus = focus;
            let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_eq!(key(&mut app, event), Action::Quit, "{focus:?}");
        }
    }

    #[test]
    fn each_harness_has_its_own_key_and_n_opens_the_default() {
        let mut app = with_one_session();
        assert_eq!(
            key(&mut app, press(KeyCode::Char('n'))),
            Action::Open(Kind::Claude)
        );
        assert_eq!(
            key(&mut app, press(KeyCode::Char('1'))),
            Action::Open(Kind::Claude)
        );
        assert_eq!(
            key(&mut app, press(KeyCode::Char('2'))),
            Action::Open(Kind::Codex)
        );
        assert_eq!(
            key(&mut app, press(KeyCode::Char('3'))),
            Action::Open(Kind::Grok)
        );
    }

    #[test]
    fn escape_abandons_the_prompt_without_sending_it() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        key(&mut app, press(KeyCode::Char('x')));
        assert_eq!(key(&mut app, press(KeyCode::Esc)), Action::Nothing);
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn a_key_release_is_not_a_second_press() {
        let mut app = with_one_session();
        let mut event = press(KeyCode::Char('q'));
        event.kind = KeyEventKind::Release;
        assert_eq!(key(&mut app, event), Action::Nothing);
    }

    #[test]
    fn a_running_session_can_still_be_typed_at() {
        // Nothing about a detached turn stops a developer thinking of the next
        // thing. Refusing would make them wait to type it.
        let mut app = with_one_session();
        app.set_running("s1", true);
        key(&mut app, press(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Compose);
    }

    #[test]
    fn backspace_deletes_and_stops_at_empty() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        key(&mut app, press(KeyCode::Char('a')));
        key(&mut app, press(KeyCode::Backspace));
        key(&mut app, press(KeyCode::Backspace));
        assert_eq!(app.composing, "");
    }

    #[test]
    fn scrolling_back_stops_at_the_newest_line() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::PageDown));
        assert_eq!(app.scrollback, 0);
        key(&mut app, press(KeyCode::PageUp));
        assert_eq!(app.scrollback, 10);
    }

    #[test]
    fn a_home_is_recorded_per_harness_and_never_shared() {
        // Resume is scoped to the profile that created the session. One home
        // used for all three would put every session in the wrong store.
        let homes = Homes {
            claude: Some("/r/claude/abc".into()),
            codex: Some("/r/codex/1".into()),
            grok: Some("/r/grok/1".into()),
        };
        let all: Vec<_> = Kind::ALL.into_iter().map(|k| homes.get(k)).collect();
        assert_eq!(all[0], Some(PathBuf::from("/r/claude/abc")));
        assert_eq!(all[1], Some(PathBuf::from("/r/codex/1")));
        assert_eq!(all[2], Some(PathBuf::from("/r/grok/1")));
    }
}
