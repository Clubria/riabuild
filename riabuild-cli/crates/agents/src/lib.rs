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
//! # A session is made by being asked something
//!
//! The rail holds sessions and *offers*, and an offer is not a session: it is a
//! sign-in a new one could be started under, with no directory, no spool and
//! nothing to count. The window used to create one per harness on the way in,
//! which reported "3 sessions" before a developer had typed anything and left
//! three directories on disk to prove it. The first prompt is what creates one
//! now — see [`drive::send`].
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

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
};
use ratatui::crossterm::{event, execute};
use ratatui::layout::Position;
use riabuild_runner::CommandRunner;
use riabuild_theme::Theme;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::sync::oneshot;

pub mod account;
pub mod activity;
pub mod app;
pub mod compose;
pub mod draw;
mod drive;
pub mod frame;
pub mod paste;
mod rollout;
pub mod store;
pub mod turn;

pub use account::{Account, SignedIn};
use app::{App, Armed, Focus, QuitKey};
use store::Store;

/// The environment variable a turn tells its harness which session it is.
///
/// riabuild's own name, not a vendor's, and it is set by [`turn`] on every turn
/// of every harness. What reads it is `riabuild internal mcp-codex`: Claude Code
/// passes its whole environment to the stdio MCP servers it spawns, so a server
/// started inside a session can name the session that started it, which the MCP
/// protocol itself provides no way to ask.
///
/// A session started outside this window — `~/.riabuild/bin/claude` in a
/// terminal — carries no such variable, and a delegation from one is a root
/// rather than a failure.
pub const DELEGATING_SESSION: &str = "RIABUILD_AGENT_SESSION";

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
    /// That checkout's repository, `owner/repo`, for the window to say so.
    ///
    /// The scoping itself is [`Store::sessions`]'s and was never this field's:
    /// a session records the checkout it was created in and the window lists
    /// only its own. What was missing was saying it out loud, which is what made
    /// the window look as though it held every agent on the machine.
    pub repo: Option<String>,
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
/// terminal, a process or a filesystem. Opening a session is not among them:
/// the prompt is what creates anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Quit,
    Send(String),
    /// Ctrl-V. Asked for here and performed in [`drive`], because reading a
    /// clipboard is a subprocess and this function is a pure map from a
    /// keypress to an intention — the same reason sending a prompt is
    /// [`Action::Send`] rather than a turn started from inside the keymap.
    Paste,
    /// Esc, on a session that is still working: ask its turn to stop rather
    /// than leave the pane for the rail. Asked for here and performed in
    /// [`drive`], because a turn is a process this window has no handle to —
    /// the only way to reach it is a file, and writing one is IO the keymap
    /// does not do.
    Interrupt,
}

/// The keymap.
pub fn key(app: &mut App, event: KeyEvent) -> Action {
    key_at(app, event, Instant::now())
}

/// The keymap, at a given moment — which only the quit confirmation reads, and
/// which is a parameter so its five seconds are testable without waiting them.
pub fn key_at(app: &mut App, event: KeyEvent, now: Instant) -> Action {
    // Windows sends both press and release; without this every key acts twice.
    // Harmless on the two platforms riabuild supports and wrong to leave out.
    if event.kind == KeyEventKind::Release {
        return Action::Nothing;
    }
    // A notice is the answer to the last key, so the next key is what it stops
    // being true for. Cleared here rather than on a timer: a message that
    // vanishes on its own is one a developer can miss entirely, and one that
    // stays is still on screen after the thing it described was undone.
    app.notice = None;

    // Ctrl-C from anywhere, and `q` on the rail — in the pane `q` is a letter.
    // Either one asks, and a second press of either within `QUIT_WINDOW`
    // leaves: one stray key used to close a window a developer was in the
    // middle of using. A developer who has just typed half a prompt still
    // expects Ctrl-C to work, and leaving interrupts nothing, because the turn
    // is not this process's child.
    let quit =
        if event.modifiers.contains(KeyModifiers::CONTROL) && event.code == KeyCode::Char('c') {
            Some(QuitKey::CtrlC)
        } else if app.focus == Focus::List && event.code == KeyCode::Char('q') {
            Some(QuitKey::Q)
        } else {
            None
        };
    app.expire_quit(now);
    if let Some(which) = quit {
        if app.armed.is_some() {
            return Action::Quit;
        }
        app.armed = Some(Armed {
            key: which,
            at: now,
        });
        return Action::Nothing;
    }
    // Any other key is the developer getting on with something, so the next
    // quit key starts over rather than confirming a question they moved past.
    app.armed = None;

    match app.focus {
        Focus::List => list_key(app, event.code),
        Focus::Session => session_key(app, event),
    }
}

/// The rail. Up and down move between sessions and offers, and only here.
fn list_key(app: &mut App, code: KeyCode) -> Action {
    match code {
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            app.select_next();
            Action::Nothing
        }
        KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
            app.select_previous();
            Action::Nothing
        }
        // One keypress into the pane, and typing works immediately once there.
        // It used to take two — one to reach the session, one to reach its box —
        // which is a confirmation of a decision the cursor had already made.
        // The caret goes to the end of whatever is already written there, which
        // is where somebody coming back to a draft carries on typing.
        //
        // Only where the cursor is on something. A window with no sessions and
        // nothing signed in has no row, and a box that took a prompt there would
        // be taking one it has nowhere to send.
        KeyCode::Right | KeyCode::Enter if app.row().is_some() => {
            app.enter();
            Action::Nothing
        }
        _ => Action::Nothing,
    }
}

/// A session, or an offer about to become one. Every character typed here goes
/// into the box; the arrows keep their meanings around it.
///
/// The one sub-keymap given the whole [`KeyEvent`] rather than its `KeyCode`,
/// because it is the only one with a text field: everywhere else a modifier
/// changes nothing, and here dropping it is the difference between Ctrl-V and
/// a literal `v` typed into the developer's prompt.
fn session_key(app: &mut App, event: KeyEvent) -> Action {
    if let Some(action) = editing_key(app, event) {
        return action;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return match event.code {
            KeyCode::Char('v') => Action::Paste,
            // Every other control key inserts nothing. Without this arm the
            // bare letter goes in the box, so a developer reaching for their
            // terminal's own Ctrl-something finds it typed into their prompt
            // instead of ignored.
            _ => Action::Nothing,
        };
    }
    match event.code {
        // Stop, not leave, while a turn is running — asked by the lock
        // (`Pane::running`) rather than by `Pane::state`, because
        // `usage_limited` overrides the *displayed* state to `Trouble` while
        // Codex is still retrying through an exhausted subscription, and the
        // turn is exactly as reachable then as it is on an ordinary busy
        // session. Once nothing is running, Esc goes back to leaving the box.
        KeyCode::Esc if app.selected().is_some_and(|pane| pane.running) => Action::Interrupt,
        KeyCode::Esc => {
            app.focus = Focus::List;
            Action::Nothing
        }
        // The one exception to "typed characters go in the box", and the one the
        // developer asked for by name: a caret at position 0 has nowhere further
        // left to go inside the line, so the next left is out of it.
        KeyCode::Left => {
            if app.compose.at_start() {
                app.focus = Focus::List;
            } else {
                app.compose.left();
            }
            Action::Nothing
        }
        KeyCode::Right => {
            app.compose.right();
            Action::Nothing
        }
        KeyCode::Home => {
            app.compose.start();
            Action::Nothing
        }
        KeyCode::End => {
            app.compose.end();
            Action::Nothing
        }
        // `PageUp` is `Fn` and an arrow on a laptop, which made scrolling a
        // gesture half the keyboards in the room could not perform. Both work.
        KeyCode::Up => {
            app.scrollback = app.scrollback.saturating_add(1);
            Action::Nothing
        }
        KeyCode::Down => {
            app.scrollback = app.scrollback.saturating_sub(1);
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
        KeyCode::Tab => {
            app.select_next();
            Action::Nothing
        }
        KeyCode::BackTab => {
            app.select_previous();
            Action::Nothing
        }
        KeyCode::Backspace => {
            app.compose.backspace();
            Action::Nothing
        }
        KeyCode::Delete => {
            app.compose.delete();
            Action::Nothing
        }
        // Sending does not leave. A conversation is a sequence of prompts, and
        // being put back in a list after each one is a keypress per turn.
        KeyCode::Enter => {
            // Shift-Enter is a line break, where the terminal can tell riabuild
            // that Shift was held — which is the kitty keyboard protocol and
            // nowhere else. Alt-Enter and Ctrl-J are the spellings that work
            // everywhere; see `editing_key`.
            if event.modifiers.contains(KeyModifiers::SHIFT) {
                app.compose.insert('\n');
                return Action::Nothing;
            }
            let text = app.compose.take().trim().to_string();
            if text.is_empty() {
                Action::Nothing
            } else {
                Action::Send(text)
            }
        }
        KeyCode::Char(ch) => {
            app.compose.insert(ch);
            Action::Nothing
        }
        _ => Action::Nothing,
    }
}

/// The editing gestures every other text field a developer uses already has:
/// jump a word, jump a line, and delete by either.
///
/// `Some` where the key was one of them, so [`session_key`] can go on treating
/// everything else exactly as it did. Tried **before** the plain arms rather
/// than inside them, because the same [`KeyCode`] means two things depending on
/// what is held down and the modifier is the whole of the difference: `←` moves
/// one character, Ctrl-`←` moves one word, Cmd-`←` goes to the start of the line.
///
/// # Three modifiers, because three of them arrive
///
/// A terminal is not a text field and does not agree with another terminal about
/// how to spell these. Ctrl-arrow arrives as `CONTROL` with the arrow under
/// xterm's `1;5D` encoding; macOS terminals with "natural text editing" turned
/// on send Option-arrow as an `ESC`-prefixed key, which crossterm reports as
/// `ALT`; and Cmd-arrow reaches a program at all only where the terminal has
/// been told to send something for it, which is usually `SUPER` under the kitty
/// keyboard protocol. So all three are accepted for what they are, and a word
/// jump is spelled every way a keyboard in this office spells it.
///
/// The same is true of backspace, in a worse way: Ctrl-Backspace has no encoding
/// of its own in the original terminal protocol, and terminals send `^H` for it —
/// which is `Ctrl-h`. That arm is not a guess about what a developer meant by
/// Ctrl-h; it is the only thing Ctrl-Backspace can arrive as outside kitty.
fn editing_key(app: &mut App, event: KeyEvent) -> Option<Action> {
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    // `SUPER` is Cmd on a Mac and the Windows key elsewhere. Both mean "the
    // whole line" in every text field on those platforms.
    let cmd = event.modifiers.contains(KeyModifiers::SUPER);
    if !(ctrl || alt || cmd) {
        return None;
    }
    match event.code {
        KeyCode::Left if cmd => app.compose.line_start(),
        KeyCode::Right if cmd => app.compose.line_end(),
        KeyCode::Left => app.compose.word_left(),
        KeyCode::Right => app.compose.word_right(),
        // The readline spelling, which is what a macOS terminal sends for
        // Option-arrow when it is not sending an arrow at all.
        KeyCode::Char('b') if alt => app.compose.word_left(),
        KeyCode::Char('f') if alt => app.compose.word_right(),
        KeyCode::Backspace if cmd => app.compose.delete_to_line_start(),
        KeyCode::Backspace => app.compose.delete_word_left(),
        // `^H`, which is what a terminal sends for Ctrl-Backspace.
        KeyCode::Char('h') if ctrl => app.compose.delete_word_left(),
        // A line break rather than a send. Enter is the send, so this is the
        // only way to put two paragraphs in one prompt — and it is why the
        // box wraps at all.
        KeyCode::Enter if alt || cmd => app.compose.insert('\n'),
        // `^J`, the other spelling of the same gesture: a terminal with no
        // kitty protocol cannot distinguish Alt-Enter from Enter, and this is
        // what its users reach for instead.
        KeyCode::Char('j') if ctrl => app.compose.insert('\n'),
        _ => return None,
    }
    // Every gesture above moves or edits the box, so the transcript goes back to
    // following the newest output — a developer who is typing has stopped
    // reading history.
    app.scrollback = 0;
    Some(Action::Nothing)
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
///
/// `clipboard` is passed in rather than found here, for the rule that keeps
/// every platform decision in the four crates allowed to make one: which
/// backend this machine needs is `riabuild-channel`'s question, already
/// answered by `clipboard::for_this_machine`. `None` is a Linux laptop with
/// neither `xclip` nor `wl-clipboard`, and Ctrl-V says so rather than failing.
///
/// `signed_in` is every sign-in the caller found signed in, sent once when the
/// last harness has answered. The window opens without waiting for it — asking
/// Claude Code is a subprocess per account — and NEW SESSION fills in when it
/// arrives. A sender dropped without answering is read as nothing signed in.
pub async fn run(
    runner: Arc<dyn CommandRunner>,
    paths: &dyn riabuild_paths::Paths,
    request: Request,
    mut signed_in: oneshot::Receiver<Vec<SignedIn>>,
    clipboard: Option<Box<dyn riabuild_channel::clipboard::Clipboard>>,
) -> Result<()> {
    let store = Store::new(paths);
    // Before anything is listed, so the cap is enforced by using the window
    // rather than by a command nobody remembers to run.
    let _ = store.prune(&request.cwd).await;
    let _ = store.prune_images().await;

    let mut app = App::new();
    let mut readers = drive::restore(&store, &request, &mut app).await?;

    if let Some(prompt) = request.prompt.clone() {
        // The one place the window waits for the answer: a prompt given on the
        // command line goes to what is signed in, so what is signed in has to be
        // known before it can go anywhere.
        app.signed_in((&mut signed_in).await.unwrap_or_default());
        drive::first_prompt(
            &store,
            runner.as_ref(),
            &request,
            &mut app,
            &mut readers,
            &prompt,
        )
        .await;
    }

    restore_terminal_on_panic();
    let mut terminal = claim(&title_for(request.repo.as_deref(), request.unicode))
        .context("could not take the terminal")?;
    // Whatever happens below, the terminal is handed back. A provisioner that
    // left a developer in raw mode on the alternate screen would be worse than
    // one that simply failed.
    let outcome = drive::drive(
        &mut terminal,
        &store,
        drive::Reach {
            runner: runner.as_ref(),
            clipboard: clipboard.as_deref(),
        },
        &request,
        &mut app,
        &mut readers,
        signed_in,
    )
    .await;
    release(&mut terminal);
    outcome
}

type Screen = Terminal<CrosstermBackend<std::io::Stdout>>;

/// What the terminal is asked to call itself while this window is open.
///
/// The repository, because a developer with four terminals open has four
/// riabuilds in them and the tab strip is the only place that can tell them
/// apart. `None` is a checkout with no GitHub remote, which is left unnamed
/// rather than guessed at.
fn title_for(repo: Option<&str>, unicode: bool) -> String {
    let dash = if unicode { "—" } else { "-" };
    match repo {
        Some(repo) => format!("riabuild agents {dash} {repo}"),
        None => "riabuild agents".to_string(),
    }
}

/// Saves the terminal's current title, and restores it.
///
/// XTWINOPS, which every terminal riabuild is used in understands and every
/// terminal that does not ignores: `22;2t` pushes the window title onto the
/// terminal's own stack, `23;2t` pops it. Without the pair, a window that named
/// itself would leave that name on the developer's tab for the rest of the
/// shell's life — there is no escape sequence that *asks* a terminal what its
/// title is, so the only way to give one back is to have never taken it.
const PUSH_TITLE: &str = "\x1b[22;2t";
const POP_TITLE: &str = "\x1b[23;2t";

fn claim(title: &str) -> Result<Screen> {
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen)?;
    // Mouse capture is deliberately **not** enabled, and that is the whole of
    // why a developer can select text in this window and their terminal copies
    // it. A program that captures the mouse receives the drag itself, and the
    // terminal's own selection — and every copy-on-select and middle-click paste
    // built on it — stops working; riabuild would then have to reimplement
    // selection, badly, in a window whose entire content is text somebody wants
    // to paste into a bug report. Nothing here reads a mouse event, so capturing
    // one bought nothing and cost that.
    write!(out, "{PUSH_TITLE}")?;
    execute!(out, SetTitle(title))?;
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

/// Hands the terminal back, and takes the window off the screen with it.
///
/// The clear is what Ctrl-C was missing. Leaving the alternate screen restores
/// the developer's scrollback *where the terminal honours one* — and tmux with
/// `alternate-screen off`, `screen`, and a `TERM` with no `smcup`/`rmcup` do
/// not, so on those the last frame simply stayed there and the shell's next
/// prompt printed on top of it. Clearing first costs nothing where the switch
/// works and is the whole of the fix where it does not.
///
/// The clear alone left those terminals blank with the shell's prompt on the
/// very last row: ratatui's `clear` puts the cursor back where it found it, and
/// where it found it was the compose line at the bottom of the window. So the
/// cursor goes home after it. Where the switch works this is invisible — leaving
/// the alternate screen restores the cursor saved on the way in — and where it
/// does not, the prompt starts at the top of a clean screen, as after `clear`.
///
/// Every step is attempted even if an earlier one failed: giving the terminal
/// back matters more than reporting why raw mode would not come off, and there
/// is nobody to report to until it has.
fn release(terminal: &mut Screen) {
    let _ = terminal.clear();
    let _ = terminal.set_cursor_position(Position::ORIGIN);
    let _ = terminal.flush();
    let _ = disable_raw_mode();
    let _ = write!(terminal.backend_mut(), "{POP_TITLE}");
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

/// Gives the terminal back if this process panics inside the window.
///
/// A panic between [`claim`] and [`release`] would otherwise leave a developer
/// in raw mode on the alternate screen with the backtrace painted somewhere they
/// cannot scroll to. Installed by [`run`] on the way in, and it chains rather
/// than replaces — the existing hook is what prints the message, and a hook that
/// swallowed it would trade a wrecked terminal for a silent crash.
fn restore_terminal_on_panic() {
    let existing = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = std::io::stdout();
        let _ = write!(out, "{POP_TITLE}");
        let _ = execute!(out, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        existing(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::first_of_each;
    use crate::app::Pane;
    use riabuild_harness::Kind;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A key with modifiers held down, for the gestures a text field has.
    fn with(modifiers: KeyModifiers, code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    /// A window with one session, focused on its box, holding `text`.
    fn writing(text: &str) -> App {
        let mut app = App::offering(first_of_each());
        app.add(Pane::new("s1".into(), Kind::Claude, "a session".into()));
        app.cursor = 0;
        app.focus = Focus::Session;
        for ch in text.chars() {
            app.compose.insert(ch);
        }
        app
    }

    #[test]
    fn a_word_jump_arrives_however_the_terminal_spells_it() {
        // Three spellings of one gesture, because three of them reach a program.
        // Ctrl-arrow is xterm's `1;5D`; a macOS terminal with natural text
        // editing on sends Option-arrow as an ESC-prefixed key, which crossterm
        // reports as Alt; and the readline `ESC b` / `ESC f` is what the same
        // terminals send when they are not sending an arrow at all.
        for event in [
            with(KeyModifiers::CONTROL, KeyCode::Left),
            with(KeyModifiers::ALT, KeyCode::Left),
            with(KeyModifiers::ALT, KeyCode::Char('b')),
        ] {
            let mut app = writing("why is the job slow");
            assert_eq!(key(&mut app, event), Action::Nothing);
            assert_eq!(app.compose.caret(), 15, "{event:?}");
        }
        for event in [
            with(KeyModifiers::CONTROL, KeyCode::Right),
            with(KeyModifiers::ALT, KeyCode::Right),
            with(KeyModifiers::ALT, KeyCode::Char('f')),
        ] {
            let mut app = writing("why is the job slow");
            app.compose.start();
            key(&mut app, event);
            assert_eq!(app.compose.caret(), 3, "{event:?}");
        }
    }

    #[test]
    fn cmd_and_the_arrows_go_to_the_ends_of_the_line() {
        let mut app = writing("first line\nsecond line");
        key(&mut app, with(KeyModifiers::SUPER, KeyCode::Left));
        assert_eq!(app.compose.caret(), 11);
        key(&mut app, with(KeyModifiers::SUPER, KeyCode::Right));
        assert_eq!(app.compose.caret(), 22);
    }

    #[test]
    fn the_two_backspaces_take_a_word_and_a_line() {
        let mut app = writing("cargo test --workspace");
        key(&mut app, with(KeyModifiers::CONTROL, KeyCode::Backspace));
        assert_eq!(app.compose.text(), "cargo test ");
        // `^H`, which is the only thing Ctrl-Backspace can arrive as outside
        // the kitty keyboard protocol.
        key(&mut app, with(KeyModifiers::CONTROL, KeyCode::Char('h')));
        assert_eq!(app.compose.text(), "cargo ");
        // Alt-Backspace is the macOS spelling of the same thing.
        key(&mut app, with(KeyModifiers::ALT, KeyCode::Backspace));
        assert_eq!(app.compose.text(), "");

        let mut app = writing("keep this\nthrow this away");
        key(&mut app, with(KeyModifiers::SUPER, KeyCode::Backspace));
        assert_eq!(app.compose.text(), "keep this\n");
    }

    #[test]
    fn a_modified_key_is_never_typed_into_the_prompt() {
        // The bug the whole `editing_key` arm sits in front of: a modifier that
        // is looked at for the arrows and dropped for the letters puts a bare
        // `b` in somebody's prompt every time they reach for Option-left.
        let mut app = writing("");
        for event in [
            with(KeyModifiers::ALT, KeyCode::Char('b')),
            with(KeyModifiers::ALT, KeyCode::Char('f')),
            with(KeyModifiers::CONTROL, KeyCode::Char('h')),
            with(KeyModifiers::CONTROL, KeyCode::Char('x')),
        ] {
            key(&mut app, event);
            assert_eq!(app.compose.text(), "", "{event:?}");
        }
    }

    #[test]
    fn enter_sends_and_the_modified_enters_break_the_line() {
        for event in [
            with(KeyModifiers::ALT, KeyCode::Enter),
            with(KeyModifiers::SHIFT, KeyCode::Enter),
            with(KeyModifiers::CONTROL, KeyCode::Char('j')),
        ] {
            let mut app = writing("cargo test");
            assert_eq!(key(&mut app, event), Action::Nothing, "{event:?}");
            assert_eq!(app.compose.text(), "cargo test\n", "{event:?}");
        }
        // and the bare one still sends, and still empties the box
        let mut app = writing("cargo test");
        assert_eq!(
            key(&mut app, press(KeyCode::Enter)),
            Action::Send("cargo test".into())
        );
        assert!(app.compose.is_empty());
    }

    #[test]
    fn the_terminal_is_told_what_this_window_is_and_which_repository() {
        assert_eq!(
            title_for(Some("Clubria/riabuild"), true),
            "riabuild agents — Clubria/riabuild"
        );
        assert_eq!(
            title_for(Some("Clubria/riabuild"), false),
            "riabuild agents - Clubria/riabuild"
        );
        // A checkout with no GitHub remote is left unnamed rather than guessed.
        assert_eq!(title_for(None, true), "riabuild agents");
    }

    fn with_one_session() -> App {
        let mut app = App::offering(first_of_each());
        app.add(Pane::new("s1".into(), Kind::Claude, "a title".into()));
        app.cursor = 0;
        app
    }

    fn type_into(app: &mut App, text: &str) {
        for ch in text.chars() {
            key(app, press(KeyCode::Char(ch)));
        }
    }

    #[test]
    fn the_window_opens_on_the_rail() {
        // Where a window with nothing running has something to say. Reading an
        // empty transcript is not a resting state.
        let app = App::new();
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn one_keypress_reaches_the_box_and_the_next_character_lands_in_it() {
        // The complaint: enter, then enter again, before a letter counted.
        let mut app = with_one_session();
        assert_eq!(key(&mut app, press(KeyCode::Enter)), Action::Nothing);
        assert_eq!(app.focus, Focus::Session);
        type_into(&mut app, "hello");
        assert_eq!(app.compose.text(), "hello");
        assert_eq!(
            key(&mut app, press(KeyCode::Enter)),
            Action::Send("hello".into())
        );
        // and the box is cleared, and the developer is still in the session —
        // a conversation is a sequence of prompts.
        assert_eq!(app.compose.text(), "");
        assert_eq!(app.focus, Focus::Session);
    }

    #[test]
    fn the_right_arrow_reaches_the_box_too() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Right));
        assert_eq!(app.focus, Focus::Session);
        type_into(&mut app, "x");
        assert_eq!(app.compose.text(), "x");
    }

    #[test]
    fn left_moves_the_caret_until_there_is_nowhere_left_to_go() {
        // The rule the developer stated: the box takes every character *unless*
        // the caret is at position 0 and left is pressed.
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        type_into(&mut app, "ab");
        key(&mut app, press(KeyCode::Left));
        assert_eq!(app.compose.caret(), 1);
        assert_eq!(app.focus, Focus::Session);
        key(&mut app, press(KeyCode::Left));
        assert_eq!(app.compose.caret(), 0);
        assert_eq!(app.focus, Focus::Session);
        // Only now, with nowhere further left inside the line.
        key(&mut app, press(KeyCode::Left));
        assert_eq!(app.focus, Focus::List);
        // and what was typed survives the trip
        assert_eq!(app.compose.text(), "ab");
    }

    #[test]
    fn an_empty_prompt_sends_nothing() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        key(&mut app, press(KeyCode::Char(' ')));
        assert_eq!(key(&mut app, press(KeyCode::Enter)), Action::Nothing);
    }

    #[test]
    fn letters_are_letters_in_a_session_and_commands_in_the_rail() {
        // The bug this stops: `q` typed into a prompt quitting the program, and
        // taking the half-written message with it.
        let mut app = with_one_session();
        assert_eq!(key(&mut app, press(KeyCode::Char('q'))), Action::Nothing);
        assert_eq!(key(&mut app, press(KeyCode::Char('q'))), Action::Quit);

        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        assert_eq!(key(&mut app, press(KeyCode::Char('q'))), Action::Nothing);
        assert_eq!(key(&mut app, press(KeyCode::Char('n'))), Action::Nothing);
        assert_eq!(app.compose.text(), "qn");
    }

    #[test]
    fn the_arrows_scroll_inside_a_session_and_change_row_in_the_rail() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        key(&mut app, press(KeyCode::Up));
        key(&mut app, press(KeyCode::Up));
        assert_eq!(app.scrollback, 2);
        key(&mut app, press(KeyCode::Down));
        assert_eq!(app.scrollback, 1);
        // and never past the newest line
        key(&mut app, press(KeyCode::Down));
        key(&mut app, press(KeyCode::Down));
        assert_eq!(app.scrollback, 0);

        key(&mut app, press(KeyCode::Esc));
        assert_eq!(app.focus, Focus::List);
        key(&mut app, press(KeyCode::Down));
        assert_eq!(app.cursor, 1);
        assert_eq!(app.scrollback, 0);
    }

    #[test]
    fn the_rail_runs_over_the_sessions_and_then_the_sign_ins() {
        let mut app = with_one_session();
        // one session and three offers
        assert_eq!(app.rows(), 4);
        for expected in [1, 2, 3, 0] {
            key(&mut app, press(KeyCode::Down));
            assert_eq!(app.cursor, expected);
        }
        key(&mut app, press(KeyCode::Up));
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn a_window_with_nothing_to_select_has_no_box_to_type_into() {
        let mut app = App::offering(Vec::new());
        assert_eq!(key(&mut app, press(KeyCode::Enter)), Action::Nothing);
        assert_eq!(key(&mut app, press(KeyCode::Right)), Action::Nothing);
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn neither_n_nor_a_digit_does_anything_on_the_rail() {
        // There is no chooser and no jumping to a harness: NEW SESSION already
        // lists every signed-in sign-in, one row each.
        for ch in ['n', '1', '2', '3'] {
            let mut app = with_one_session();
            assert_eq!(key(&mut app, press(KeyCode::Char(ch))), Action::Nothing);
            assert_eq!(app.focus, Focus::List, "{ch}");
            assert_eq!(app.cursor, 0, "{ch}");
            assert_eq!(app.offers.len(), 3, "{ch}");
        }
    }

    fn typed(app: &mut App, text: &str) {
        for ch in text.chars() {
            key(app, press(KeyCode::Char(ch)));
        }
    }

    #[test]
    fn each_row_keeps_its_own_draft() {
        // The bug: one box for the whole window, so `Tab` from inside a pane
        // carried a half-written prompt to the next session, and Enter sent it
        // there — or to an offer, which started a session nobody meant.
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Right));
        typed(&mut app, "for the session");

        key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.row(), Some(app::Row::Offer(0)));
        assert!(app.compose.text().is_empty(), "{}", app.compose.text());
        typed(&mut app, "for claude");

        key(&mut app, press(KeyCode::BackTab));
        assert_eq!(app.compose.text(), "for the session");
        key(&mut app, press(KeyCode::Tab));
        assert_eq!(app.compose.text(), "for claude");
    }

    #[test]
    fn leaving_by_the_left_edge_keeps_the_draft_and_coming_back_puts_the_caret_at_its_end() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Right));
        typed(&mut app, "half a thought");
        key(&mut app, press(KeyCode::Home));
        // Caret at 0, so this `←` leaves rather than moving inside the text.
        key(&mut app, press(KeyCode::Left));
        assert_eq!(app.focus, Focus::List);

        // Browse elsewhere and come back: the draft is where it was left.
        key(&mut app, press(KeyCode::Down));
        key(&mut app, press(KeyCode::Up));
        key(&mut app, press(KeyCode::Right));
        assert_eq!(app.focus, Focus::Session);
        assert_eq!(app.compose.text(), "half a thought");
        assert_eq!(app.compose.caret(), "half a thought".chars().count());
    }

    #[test]
    fn ctrl_c_leaves_from_anywhere() {
        for focus in [Focus::List, Focus::Session] {
            let mut app = with_one_session();
            app.focus = focus;
            let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_eq!(key(&mut app, event), Action::Nothing, "{focus:?}");
            assert_eq!(key(&mut app, event), Action::Quit, "{focus:?}");
        }
    }

    #[test]
    fn escape_leaves_the_box_without_losing_what_is_in_it() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        type_into(&mut app, "half a thought");
        assert_eq!(key(&mut app, press(KeyCode::Esc)), Action::Nothing);
        assert_eq!(app.focus, Focus::List);
        assert_eq!(app.compose.text(), "half a thought");
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
        type_into(&mut app, "and another thing");
        assert_eq!(
            key(&mut app, press(KeyCode::Enter)),
            Action::Send("and another thing".into())
        );
    }

    #[test]
    fn escape_interrupts_a_running_session_instead_of_leaving_it() {
        let mut app = with_one_session();
        app.set_running("s1", true);
        key(&mut app, press(KeyCode::Enter));
        assert_eq!(key(&mut app, press(KeyCode::Esc)), Action::Interrupt);
        // And the developer is still looking at the box — an interrupt is
        // not a navigation, and there is nothing here to leave for.
        assert_eq!(app.focus, Focus::Session);

        // Once nothing is running any more, Esc goes back to its other job.
        app.set_running("s1", false);
        assert_eq!(key(&mut app, press(KeyCode::Esc)), Action::Nothing);
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn backspace_and_delete_work_around_the_caret() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        type_into(&mut app, "abc");
        key(&mut app, press(KeyCode::Left));
        key(&mut app, press(KeyCode::Backspace));
        assert_eq!(app.compose.text(), "ac");
        key(&mut app, press(KeyCode::Delete));
        assert_eq!(app.compose.text(), "a");
    }

    #[test]
    fn a_page_at_a_time_still_works_where_the_keyboard_has_the_keys() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        key(&mut app, press(KeyCode::PageDown));
        assert_eq!(app.scrollback, 0);
        key(&mut app, press(KeyCode::PageUp));
        assert_eq!(app.scrollback, 10);
    }

    fn hold(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn ctrl_v_in_a_session_asks_for_the_clipboard() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        assert_eq!(key(&mut app, hold(KeyCode::Char('v'))), Action::Paste);
        // and nothing was typed: the box is what it was.
        assert!(app.compose.is_empty());
    }

    /// The bug this arm exists to prevent. `session_key` was given the key
    /// *code* alone, so a modifier was not merely ignored — it was invisible,
    /// and Ctrl-V arrived as the letter `v` in the developer's prompt.
    #[test]
    fn a_plain_v_is_still_a_letter() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        type_into(&mut app, "give");
        assert_eq!(key(&mut app, hold(KeyCode::Char('v'))), Action::Paste);
        assert_eq!(app.compose.text(), "give");
    }

    /// Every other control key is ignored rather than half-typed. A developer
    /// reaching for their terminal's own Ctrl-W should not find a `w` in their
    /// prompt when the terminal did not take it.
    #[test]
    fn other_control_keys_type_nothing() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        for ch in ['w', 'u', 'a', 'e', 'l'] {
            assert_eq!(key(&mut app, hold(KeyCode::Char(ch))), Action::Nothing);
        }
        assert!(app.compose.is_empty());
    }

    /// Ctrl-C still leaves from inside the box, which is checked before the
    /// control arm above could swallow it.
    #[test]
    fn ctrl_c_still_leaves_from_the_compose_line() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        type_into(&mut app, "half");
        assert_eq!(key(&mut app, hold(KeyCode::Char('c'))), Action::Nothing);
        assert_eq!(key(&mut app, hold(KeyCode::Char('c'))), Action::Quit);
        // Asking did not type a `c`.
        assert_eq!(app.compose.text(), "half");
    }

    /// One `q` or Ctrl-C asks; a second within five seconds leaves. Time is
    /// passed in, so the window is tested without waiting it out.
    #[test]
    fn quitting_takes_a_second_press_within_five_seconds() {
        let start = Instant::now();
        let later = |seconds: u64| start + std::time::Duration::from_secs(seconds);

        // Confirmed in time.
        let mut app = with_one_session();
        assert_eq!(
            key_at(&mut app, press(KeyCode::Char('q')), start),
            Action::Nothing
        );
        assert_eq!(app.armed.map(|armed| armed.key), Some(QuitKey::Q));
        assert_eq!(
            key_at(&mut app, press(KeyCode::Char('q')), later(4)),
            Action::Quit
        );

        // Too late: the second press asks again rather than leaving.
        let mut app = with_one_session();
        key_at(&mut app, press(KeyCode::Char('q')), start);
        assert_eq!(
            key_at(&mut app, press(KeyCode::Char('q')), later(5)),
            Action::Nothing
        );
        assert_eq!(app.armed.map(|armed| armed.at), Some(later(5)));
        assert_eq!(
            key_at(&mut app, press(KeyCode::Char('q')), later(6)),
            Action::Quit
        );

        // The redraw tick takes the message away on time, with no key pressed.
        let mut app = with_one_session();
        key_at(&mut app, hold(KeyCode::Char('c')), start);
        app.expire_quit(later(4));
        assert!(app.armed.is_some());
        app.expire_quit(later(5));
        assert_eq!(app.armed, None);
    }

    /// Either quit key confirms the other, and any other key starts over.
    #[test]
    fn either_quit_key_confirms_and_anything_else_disarms() {
        let now = Instant::now();
        let mut app = with_one_session();
        key_at(&mut app, press(KeyCode::Char('q')), now);
        assert_eq!(
            key_at(&mut app, hold(KeyCode::Char('c')), now),
            Action::Quit
        );

        let mut app = with_one_session();
        key_at(&mut app, press(KeyCode::Char('q')), now);
        key_at(&mut app, press(KeyCode::Down), now);
        assert_eq!(app.armed, None);
        assert_eq!(
            key_at(&mut app, press(KeyCode::Char('q')), now),
            Action::Nothing
        );

        // In the pane `q` is a letter, so it disarms a Ctrl-C rather than
        // confirming it — and is typed.
        let mut app = with_one_session();
        key_at(&mut app, press(KeyCode::Enter), now);
        key_at(&mut app, hold(KeyCode::Char('c')), now);
        assert_eq!(
            key_at(&mut app, press(KeyCode::Char('q')), now),
            Action::Nothing
        );
        assert_eq!(app.armed, None);
        assert_eq!(app.compose.text(), "q");

        // A release is not a keypress and changes nothing.
        let mut app = with_one_session();
        key_at(&mut app, press(KeyCode::Char('q')), now);
        let mut release = press(KeyCode::Down);
        release.kind = KeyEventKind::Release;
        key_at(&mut app, release, now);
        assert_eq!(
            key_at(&mut app, press(KeyCode::Char('q')), now),
            Action::Quit
        );
    }

    /// A notice is the answer to one key, so the next key is what it stops
    /// being true for — never a timer, which either takes the message away
    /// before it is read or leaves it up after it is wrong.
    #[test]
    fn a_notice_lasts_until_the_next_keypress() {
        let mut app = with_one_session();
        key(&mut app, press(KeyCode::Enter));
        app.notice = Some("Nothing on the clipboard to paste.".into());
        // Not cleared by the redraw tick, which does not go through the keymap.
        assert!(app.notice.is_some());
        key(&mut app, press(KeyCode::Char('a')));
        assert_eq!(app.notice, None);
    }
}
