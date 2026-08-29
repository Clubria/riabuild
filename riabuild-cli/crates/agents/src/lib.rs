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

pub mod account;
pub mod app;
pub mod draw;
pub mod store;
pub mod turn;

pub use account::{Account, Accounts};
use app::{App, Focus, Pane};
use store::Store;

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
    /// Every sign-in a session in this window may be started under —
    /// `claude-1` … `claude-9`, and the same for Codex and Grok Build.
    ///
    /// Resolved by the caller from riabuild's own account list, and the one that
    /// was chosen is recorded on the session when it is created — never
    /// recomputed. A session is only resumable under the profile that made it,
    /// so if the primary Claude account changes between turns, a recomputed home
    /// would point at a different store and the conversation would silently
    /// start over as a new one.
    pub accounts: Accounts,
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
    /// Start a session under this sign-in.
    Open(Account),
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
                app.focus = Focus::Transcript;
                Action::Nothing
            }
            KeyCode::Enter => {
                let text = app.composing.trim().to_string();
                app.composing.clear();
                app.focus = Focus::Transcript;
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
        // Choosing which sign-in a new session runs under. Escape is the way
        // out of it, and choosing nothing is always possible: this is the one
        // screen that appears because a developer asked a question, so it must
        // be answerable with "never mind".
        Focus::Picker => match event.code {
            KeyCode::Esc => {
                app.focus = Focus::Transcript;
                Action::Nothing
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.pick_next();
                Action::Nothing
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.pick_previous();
                Action::Nothing
            }
            KeyCode::Enter => match app.picked().cloned() {
                Some(account) => {
                    app.focus = Focus::Transcript;
                    Action::Open(account)
                }
                // No accounts at all. Nothing to open, and refusing silently is
                // better than opening a session under a home nobody has.
                None => Action::Nothing,
            },
            _ => Action::Nothing,
        },
        // The session column. Up and down mean "another session" only here,
        // which is the whole of what the left arrow bought.
        Focus::Sessions => match event.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                app.select_next();
                Action::Nothing
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                app.select_previous();
                Action::Nothing
            }
            // Three ways back, because there is nothing to confirm: moving the
            // cursor has already switched session, so every one of these is
            // "done here" rather than "accept".
            KeyCode::Right | KeyCode::Enter | KeyCode::Esc => {
                app.focus = Focus::Transcript;
                Action::Nothing
            }
            KeyCode::Char('n') => {
                app.open_picker();
                Action::Nothing
            }
            _ => Action::Nothing,
        },
        // Reading. The resting state, so the arrows scroll what is in front of
        // the developer rather than moving between sessions — `PageUp` is `Fn`
        // and an arrow on a laptop, which made scrolling a gesture half the
        // keyboards in the room could not perform.
        Focus::Transcript => match event.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Left => {
                app.focus = Focus::Sessions;
                Action::Nothing
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.scrollback = app.scrollback.saturating_add(1);
                Action::Nothing
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.scrollback = app.scrollback.saturating_sub(1);
                Action::Nothing
            }
            // Kept, rather than relied on: a full-size keyboard has them and a
            // page is a faster way through a long transcript than a line.
            KeyCode::PageUp => {
                app.scrollback = app.scrollback.saturating_add(10);
                Action::Nothing
            }
            KeyCode::PageDown => {
                app.scrollback = app.scrollback.saturating_sub(10);
                Action::Nothing
            }
            // Tab still cycles sessions without leaving the transcript, which is
            // what a developer watching two agents at once actually does.
            KeyCode::Tab => {
                app.select_next();
                Action::Nothing
            }
            KeyCode::BackTab => {
                app.select_previous();
                Action::Nothing
            }
            KeyCode::Char('n') => {
                app.open_picker();
                Action::Nothing
            }
            // The quick way to a new session on a harness's first sign-in, for
            // a developer who has one of each and does not want a chooser.
            KeyCode::Char('1') => open_first(app, Kind::Claude),
            KeyCode::Char('2') => open_first(app, Kind::Codex),
            KeyCode::Char('3') => open_first(app, Kind::Grok),
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

/// A new session on a harness's first sign-in.
///
/// Nothing at all where that harness has no accounts, which is only reachable
/// on a machine where riabuild has not finished setting one up.
fn open_first(app: &App, kind: Kind) -> Action {
    match app.accounts.first(kind) {
        Some(account) => Action::Open(account.clone()),
        None => Action::Nothing,
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

    let mut app = App::new(request.accounts.clone());
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
        pane.account = record.account;
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

    // Every harness riabuild installs gets a pane, on its first sign-in. One
    // that already has a session keeps it — reopening should hand a developer
    // back the conversation they were having, not a fourth empty pane beside it.
    //
    // One pane per *harness* and not one per account: nine sign-ins each is
    // twenty-seven panes, which is a list nobody can read, for a developer who
    // is using two of them. The other twenty-four are a keypress away in the
    // chooser rather than on screen from the start.
    for kind in Kind::ALL {
        if app.panes.iter().any(|pane| pane.kind == kind) {
            continue;
        }
        // A harness with no accounts still gets its pane, under no home at all —
        // which is what this did for all three before accounts were offered.
        let account = request
            .accounts
            .first(kind)
            .cloned()
            .unwrap_or_else(|| Account::new(kind, 1, None));
        let record = store.create(&account, &request.cwd).await?;
        readers.insert(record.id.clone(), Reader::new(kind));
        let mut pane = Pane::new(record.id, kind, String::new());
        pane.account = account.number;
        app.add(pane);
    }
    app.selected = 0;
    Ok(())
}

type Screen = Terminal<CrosstermBackend<std::io::Stdout>>;

fn claim() -> Result<Screen> {
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    // The alternate screen is not a blank one, and ratatui only writes the cells
    // that differ from the frame before. On the very first draw the frame before
    // is *ratatui's* idea of blank rather than the terminal's, so every cell this
    // interface leaves empty — the margins, the gaps between panes, everything
    // below a short transcript — is never written at all, and whatever the
    // terminal already had there stays on screen underneath. That is old shell
    // history showing through the window, and it looks like a rendering bug
    // because it is one.
    //
    // `Terminal::new` does not do this and `ratatui::init` does; riabuild claims
    // the terminal by hand, so it has to. Resizing is already covered —
    // `autoresize` clears on every size change — which is why the symptom
    // vanished the moment anyone dragged the window and never came back.
    terminal.clear()?;
    // Paired with the `show_cursor` in `release`. The compose line draws its own
    // block where the caret belongs; the terminal's real one would sit wherever
    // the last cell was written and blink there through every redraw.
    terminal.hide_cursor()?;
    Ok(terminal)
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
            Action::Open(account) => {
                let record = store.create(&account, &request.cwd).await?;
                readers.insert(record.id.clone(), Reader::new(account.kind));
                let mut pane = Pane::new(record.id, account.kind, String::new());
                pane.account = account.number;
                app.add(pane);
                app.selected = app.panes.len().saturating_sub(1);
                // Straight to reading the new session rather than leaving the
                // cursor in the column it was opened from.
                app.scrollback = 0;
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

    /// Every sign-in riabuild keeps, which is what the window is handed.
    fn every_account() -> Accounts {
        let mut all = Vec::new();
        for kind in Kind::ALL {
            for number in 1..=9 {
                all.push(Account::new(kind, number, Some(PathBuf::from("/r"))));
            }
        }
        Accounts::from(all)
    }

    fn with_one_session() -> App {
        let mut app = App::new(every_account());
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
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn an_empty_prompt_sends_nothing() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        key(&mut app, press(KeyCode::Char(' ')));
        assert_eq!(key(&mut app, press(KeyCode::Enter)), Action::Nothing);
    }

    #[test]
    fn the_arrows_scroll_what_is_being_read_and_the_left_one_leaves_it() {
        // `PageUp` is `Fn` and an arrow on a laptop keyboard. Scrolling was the
        // one thing only those two keys did, which made the main gesture of this
        // screen one half the keyboards in the room cannot perform.
        let mut app = with_one_session();
        assert_eq!(app.focus, Focus::Transcript);
        key(&mut app, press(KeyCode::Up));
        key(&mut app, press(KeyCode::Up));
        assert_eq!(app.scrollback, 2);
        key(&mut app, press(KeyCode::Down));
        assert_eq!(app.scrollback, 1);
        // and never past the newest line
        key(&mut app, press(KeyCode::Down));
        key(&mut app, press(KeyCode::Down));
        assert_eq!(app.scrollback, 0);

        // Left is where sessions live, and only there do the arrows change one.
        key(&mut app, press(KeyCode::Left));
        assert_eq!(app.focus, Focus::Sessions);
        key(&mut app, press(KeyCode::Up));
        assert_eq!(app.scrollback, 0);
        key(&mut app, press(KeyCode::Right));
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn moving_between_sessions_happens_in_the_column_and_nowhere_else() {
        let mut app = with_one_session();
        app.add(Pane::new("s2".into(), Kind::Codex, String::new()));
        key(&mut app, press(KeyCode::Left));
        key(&mut app, press(KeyCode::Down));
        assert_eq!(app.selected, 1);
        key(&mut app, press(KeyCode::Up));
        assert_eq!(app.selected, 0);
        // Enter is "done here" rather than "accept": the cursor has already
        // switched session, so there is nothing left to confirm.
        assert_eq!(key(&mut app, press(KeyCode::Enter)), Action::Nothing);
        assert_eq!(app.focus, Focus::Transcript);
        // and tab still switches without going over there at all
        key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn a_new_session_can_be_opened_under_any_sign_in() {
        // The bug: `n` opened `claude-1` and there was no way to reach the other
        // eight, or any of Codex's or Grok Build's nine, from this window.
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Char('n')));
        assert_eq!(app.focus, Focus::Picker);
        // On the sign-in the selected session is already running under, so
        // "another one of these" is one keypress.
        assert_eq!(app.picked().map(Account::name).as_deref(), Some("claude-1"));

        for _ in 0..3 {
            key(&mut app, press(KeyCode::Down));
        }
        assert_eq!(app.picked().map(Account::name).as_deref(), Some("claude-4"));
        let opened = key(&mut app, press(KeyCode::Enter));
        assert_eq!(
            opened,
            Action::Open(Account::new(Kind::Claude, 4, Some(PathBuf::from("/r"))))
        );
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn the_chooser_can_be_left_without_opening_anything() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Char('n')));
        key(&mut app, press(KeyCode::Down));
        assert_eq!(key(&mut app, press(KeyCode::Esc)), Action::Nothing);
        assert_eq!(app.focus, Focus::Transcript);
    }

    #[test]
    fn a_window_with_no_accounts_opens_nothing_rather_than_guessing() {
        // Reachable on a machine riabuild has not finished setting up. A session
        // under an invented home would resume from a store nobody has.
        let mut app = App::new(Accounts::default());
        app.add(Pane::new("s1".into(), Kind::Claude, String::new()));
        key(&mut app, press(KeyCode::Char('n')));
        assert_eq!(key(&mut app, press(KeyCode::Enter)), Action::Nothing);
        assert_eq!(key(&mut app, press(KeyCode::Char('1'))), Action::Nothing);
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
        for focus in [
            Focus::Transcript,
            Focus::Sessions,
            Focus::Compose,
            Focus::Picker,
        ] {
            let mut app = with_one_session();
            app.focus = focus;
            let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_eq!(key(&mut app, event), Action::Quit, "{focus:?}");
        }
    }

    #[test]
    fn each_harness_has_a_key_that_opens_its_first_sign_in() {
        // For the developer with one account each, who should not be asked to
        // choose from twenty-seven of them to get a second Codex.
        for (digit, kind) in [('1', Kind::Claude), ('2', Kind::Codex), ('3', Kind::Grok)] {
            let mut app = with_one_session();
            let Action::Open(account) = key(&mut app, press(KeyCode::Char(digit))) else {
                panic!("{digit} opened nothing");
            };
            assert_eq!(account.kind, kind);
            assert_eq!(account.number, 1);
        }
    }

    #[test]
    fn escape_abandons_the_prompt_without_sending_it() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        key(&mut app, press(KeyCode::Char('x')));
        assert_eq!(key(&mut app, press(KeyCode::Esc)), Action::Nothing);
        assert_eq!(app.focus, Focus::Transcript);
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
    fn a_page_at_a_time_still_works_where_the_keyboard_has_the_keys() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::PageDown));
        assert_eq!(app.scrollback, 0);
        key(&mut app, press(KeyCode::PageUp));
        assert_eq!(app.scrollback, 10);
    }

    #[test]
    fn a_home_is_recorded_per_account_and_never_shared() {
        // Resume is scoped to the profile that created the session. One home
        // used for two accounts would put both conversations in one store, and
        // the second would resume the first's.
        let accounts = Accounts::from(vec![
            Account::new(Kind::Claude, 1, Some("/r/claude/abc".into())),
            Account::new(Kind::Claude, 2, Some("/r/claude/def".into())),
            Account::new(Kind::Codex, 1, Some("/r/codex/1".into())),
            Account::new(Kind::Grok, 1, Some("/r/grok/1".into())),
        ]);
        // A harness opens on its first account, never on whichever came first
        // in the list.
        assert_eq!(
            accounts.first(Kind::Claude).and_then(|a| a.home.clone()),
            Some(PathBuf::from("/r/claude/abc"))
        );
        assert_eq!(
            accounts.first(Kind::Codex).and_then(|a| a.home.clone()),
            Some(PathBuf::from("/r/codex/1"))
        );
        let homes: Vec<_> = accounts.all().iter().map(|a| a.home.clone()).collect();
        assert_eq!(homes[0], Some(PathBuf::from("/r/claude/abc")));
        assert_ne!(homes[0], homes[1]);
    }
}
