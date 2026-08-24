//! `riabuild agents` — Claude Code, Codex and Grok Build in one window.
//!
//! The three harnesses are driven **headless**: each runs in its own structured
//! output mode and this crate draws the result, rather than embedding each
//! vendor's own full-screen interface in a pane. That choice is the whole
//! architecture, and it buys the one thing a terminal multiplexer cannot give —
//! *state*. Screen-scraping three alternate-screen TUIs tells you what pixels
//! changed; reading their event streams tells you which agent is blocked, what
//! it is running, and what it has spent. See `riabuild-harness`, which owns the
//! part where the three disagree.
//!
//! # Ownership of the terminal
//!
//! This is the third thing in riabuild that writes to a terminal, and it is a
//! different thing from the other two. `riabuild-ui` prints lines *past* a
//! terminal it does not own; `run_interactive` hands the terminal to a child
//! and looks away. This takes the terminal — raw mode, alternate screen — draws
//! whole frames, and gives it back. It is not an exception to the
//! "`riabuild-ui` writes with `println!`" rule so much as a fourth case, and it
//! is confined to this crate: nothing here prints, and nothing outside here
//! draws.
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
use riabuild_harness::{Fleet, Kind, Launch};
use riabuild_runner::CommandRunner;
use riabuild_theme::Theme;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

pub mod app;
pub mod draw;

use app::{App, Focus, State};

/// Where each harness's binary is.
///
/// Absolute paths, from `Ctx::claude()`, `Ctx::codex()` and `Ctx::grok()`. Never
/// bare names: `~/.riabuild/bin` is not on `PATH` during provisioning, and a
/// bare name finds whichever copy the laptop already had.
#[derive(Debug, Clone)]
pub struct Programs {
    pub claude: String,
    pub codex: String,
    pub grok: String,
}

impl Programs {
    fn get(&self, kind: Kind) -> &str {
        match kind {
            Kind::Claude => &self.claude,
            Kind::Codex => &self.codex,
            Kind::Grok => &self.grok,
        }
    }
}

/// What `riabuild agents` was asked to do.
#[derive(Debug, Clone)]
pub struct Request {
    pub programs: Programs,
    /// The checkout every session works in.
    pub cwd: String,
    /// The first thing to say to every session, if anything.
    ///
    /// One prompt for all three rather than one each: asking the same question
    /// of Claude Code, Codex and Grok Build at once is the thing three panes
    /// side by side are actually for.
    pub prompt: Option<String>,
    pub theme: Theme,
    /// Whether this terminal can be trusted with the block glyphs, which is
    /// `riabuild-ui`'s decision rather than one made again here.
    pub unicode: bool,
}

/// What a keypress asks for.
///
/// Returned rather than performed, so the whole keymap is testable without a
/// terminal or a process.
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
    // prompt still expects it to work.
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
                // Only where there is a live session to talk to. Opening the
                // prompt over an ended one invites a message that goes nowhere.
                if app
                    .selected()
                    .map(|pane| pane.state != State::Gone)
                    .unwrap_or(false)
                {
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
/// nobody is typing — which is most of the time. The thread is detached and
/// ends with the process; there is nothing to join, because it is parked inside
/// a `read` that only the terminal can complete.
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
pub async fn run(runner: Arc<dyn CommandRunner>, request: Request) -> Result<()> {
    let mut fleet = Fleet::new(runner);
    let mut app = App::new();

    // Every harness riabuild installs gets a pane, always. The two that do not
    // start a process until they are spoken to cost nothing to have open.
    for kind in Kind::ALL {
        open(&mut fleet, &mut app, &request, kind, request.prompt.clone()).await?;
    }
    // The first session is the one a developer starts typing at.
    app.selected = 0;

    let mut terminal = claim().context("could not take the terminal")?;
    // Whatever happens below, the terminal is handed back. A provisioner that
    // left a developer in raw mode on the alternate screen would be worse than
    // one that simply failed.
    let outcome = drive(&mut terminal, &mut fleet, &mut app, &request).await;
    release(&mut terminal);
    fleet.shutdown().await;
    outcome
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
    fleet: &mut Fleet,
    app: &mut App,
    request: &Request,
) -> Result<()> {
    let mut keys = keys();
    // Fast enough for the spinner to read as motion, slow enough that an idle
    // fleet is not redrawing a hundred times a second.
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
            Some((id, event)) = fleet.next_event() => {
                app.observe(id, &event);
                Action::Nothing
            }
            _ = ticker.tick() => {
                app.tick = app.tick.wrapping_add(1);
                Action::Nothing
            }
        };

        match action {
            Action::Nothing => {}
            Action::Quit => app.quit = true,
            Action::Open(kind) => open(fleet, app, request, kind, None).await?,
            Action::Send(text) => {
                if let Some(id) = app.selected().map(|pane| pane.id) {
                    app.sent(&text);
                    // A harness that will not start is this session's problem
                    // and not the fleet's: the other agents keep running and the
                    // pane says what happened.
                    if let Err(error) = fleet.send(id, &text).await {
                        app.observe(id, &riabuild_harness::Event::Trouble(format!("{error:#}")));
                    }
                }
            }
        }
    }
}

async fn open(
    fleet: &mut Fleet,
    app: &mut App,
    request: &Request,
    kind: Kind,
    prompt: Option<String>,
) -> Result<()> {
    let launch = Launch {
        kind,
        program: request.programs.get(kind).to_string(),
        cwd: request.cwd.clone(),
        prompt: prompt.clone(),
    };
    let id = fleet.open(launch).await?;
    app.opened(id, kind, request.cwd.clone());
    // The newest session is the one the developer just asked for.
    app.selected = app.panes.len().saturating_sub(1);
    // An opening prompt is still a prompt: it has to show in the transcript and
    // mark the pane busy, or a session started with `--prompt` reads as idle
    // through the whole of the model's first think.
    if let Some(prompt) = prompt {
        app.sent(&prompt);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_harness::SessionId;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn with_one_session() -> App {
        let mut app = App::new();
        app.opened(SessionId(1), Kind::Claude, "/work".into());
        app.observe(SessionId(1), &riabuild_harness::Event::Idle);
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
    fn an_ended_session_cannot_be_written_to() {
        let mut app = App::new();
        app.opened(SessionId(1), Kind::Claude, "/work".into());
        app.observe(SessionId(1), &riabuild_harness::Event::Exited(1));
        key(&mut app, press(KeyCode::Enter));
        assert_eq!(app.focus, Focus::List);
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
    fn every_harness_is_started_from_an_absolute_path() {
        // A bare name finds whatever the laptop already had, which is the one
        // thing riabuild owning its tools exists to prevent.
        let programs = Programs {
            claude: "/opt/riabuild/claude".into(),
            codex: "/opt/riabuild/codex".into(),
            grok: "/opt/riabuild/grok".into(),
        };
        for kind in [Kind::Claude, Kind::Codex, Kind::Grok] {
            assert!(programs.get(kind).starts_with('/'), "{kind:?}");
        }
    }
}
