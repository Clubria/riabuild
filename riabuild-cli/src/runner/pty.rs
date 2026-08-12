//! A child that prints through riabuild rather than over it.
//!
//! `run_interactive` normally hands the terminal to the child and looks away —
//! the handoff `CLAUDE.md` exempts from the async-IO rule. A subdued child does
//! not get that. It gets a pseudo-terminal riabuild owns, its output goes
//! through [`Subdue`], and what survives is printed as dimmed lines at the
//! depth of a note.
//!
//! Unix only, which is the whole supported surface. Every read and write here
//! goes through `AsyncFd` on the current-thread runtime: a subdued
//! `sudo apt-get` holds this loop for as long as the developer takes to type a
//! password, and a blocking read would hold the reactor with it.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::unix::AsyncFd;
use tokio::process::{Child, Command};
use tokio::signal::unix::{SignalKind, signal};

use super::subdue::Subdue;
use crate::theme::{Role, Theme};

/// The indent child output is printed at — `ui::note`'s, because that is what
/// a line from a child is: a note under the task that started it.
const INDENT: &str = "    ";

/// How long to keep reading after the child has exited.
///
/// Whatever it printed just before exiting is still in the pty buffer. The read
/// normally ends immediately with EOF; the bound is there because an orphan
/// holding the slave open would otherwise turn "finished" into a provisioner
/// that hangs with no output, which is the failure this codebase is written
/// against.
const DRAIN: Duration = Duration::from_millis(250);

/// Whether a pty can be had at all: both ends of the developer's terminal.
pub(super) fn available() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Runs `command` under a pty, printing what it says as subdued lines.
pub(super) async fn run(mut command: Command, theme: Theme, program: &str) -> Result<i32> {
    let (master, slave) = open(&winsize())?;

    // Each of the child's three descriptors needs its own handle: `Stdio` takes
    // ownership of what it is given. All three are the pty, so nothing the
    // child writes can reach the terminal except through the filter.
    command.stdin(stdio(&slave)?);
    command.stdout(stdio(&slave)?);
    command.stderr(stdio(&slave)?);

    let master_fd = master.as_raw_fd();
    // SAFETY: the closure runs between fork and exec in a single-threaded
    // child. `setsid`, `ioctl` and `close` are all async-signal-safe, and
    // nothing here allocates or takes a lock.
    unsafe {
        command.pre_exec(move || {
            // A new session, then claim the slave as this session's
            // *controlling* terminal. Without it the pty is three ordinary
            // descriptors, `sudo` finds no controlling terminal, and it refuses
            // to prompt rather than asking for a password nobody could type.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // fd 0 is the slave by now: `pre_exec` runs after the stdio dup2s.
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // A child holding the master end is what would keep a read on it
            // from ever reporting EOF.
            libc::close(master_fd);
            Ok(())
        });
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("could not start `{program}`"))?;
    // The parent's copy of the slave, for the same reason: while it is open the
    // master never reports EOF, and the drain below would wait out its bound on
    // every single command.
    drop(slave);

    // Everything from here to the `drop` is printed through the painter, so the
    // terminal stays raw for exactly as long as the child owns it.
    let restore = Restore::take()?;
    let outcome = pump(&mut child, master, theme).await;
    drop(restore);

    outcome.with_context(|| format!("`{program}` did not finish"))
}

/// Renders subdued lines, and remembers how much of the terminal the last
/// unterminated one occupied.
struct Painter {
    theme: Theme,
    /// Columns written by the last `partial`, still on screen. A shorter redraw
    /// has to cover them or the tail of the longer frame stays visible.
    open: usize,
    /// What that partial said, so an unchanged repaint writes nothing.
    last: String,
}

impl Painter {
    fn new(theme: Theme) -> Self {
        Self {
            theme,
            open: 0,
            last: String::new(),
        }
    }

    /// A finished line. Ends with a newline; nothing is left open.
    fn line(&mut self, text: &str) -> String {
        let out = self.draw(text);
        self.open = 0;
        self.last.clear();
        out + "\n"
    }

    /// The line as it currently stands, with the child still writing it.
    ///
    /// Empty when nothing has changed. The pump repaints after every read, and
    /// a child that prints `Password: ` and then waits must not have it
    /// reprinted on every wakeup.
    fn partial(&mut self, text: &str) -> String {
        if text == self.last {
            return String::new();
        }
        let out = self.draw(text);
        self.open = text.chars().count();
        self.last = text.to_string();
        out
    }

    /// The same idiom `ui::applied` uses over a status line: return to the
    /// start, write, and pad over whatever the longer previous frame left.
    fn draw(&self, text: &str) -> String {
        let padding = " ".repeat(self.open.saturating_sub(text.chars().count()));
        format!("\r{INDENT}{}{padding}", self.theme.paint(Role::Muted, text))
    }
}

/// How wide the child is told its terminal is.
///
/// The real width less the indent, so a child that wraps at the width it was
/// given does not push every wrapped line past the right edge. Never zero: a
/// terminal of no width makes some children divide by it.
fn child_columns(terminal: u16) -> u16 {
    terminal.saturating_sub(INDENT.len() as u16).max(1)
}

/// The developer's terminal, put back the way it was found.
///
/// Raw mode and `O_NONBLOCK` are changes to a file description the shell
/// shares. Restoring them on an early return is not tidiness: a terminal left
/// raw with no echo outlives the process that did it, and the developer's next
/// command is typed into a shell that shows them nothing.
struct Restore {
    termios: libc::termios,
    flags: libc::c_int,
}

impl Drop for Restore {
    fn drop(&mut self) {
        // SAFETY: both calls take the values read from this same descriptor.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.termios);
            libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, self.flags);
        }
    }
}

impl Restore {
    /// Puts the terminal into raw mode and fd 0 into non-blocking mode,
    /// returning the guard that undoes both.
    ///
    /// `O_NONBLOCK` is what lets `AsyncFd` watch fd 0. The alternative,
    /// `tokio::io::stdin()`, reads on a blocking thread that cannot be
    /// cancelled — so a keystroke typed after the child exits would be
    /// swallowed by a read nobody is waiting for, and the next `ui::ask` would
    /// lose it.
    fn take() -> Result<Self> {
        // SAFETY: fd 0 is a terminal — `available()` checked — and every
        // pointer below is to a local this function owns.
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut termios) != 0 {
                return Err(std::io::Error::last_os_error()).context("could not read the terminal");
            }
            let flags = libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL);
            if flags < 0 {
                return Err(std::io::Error::last_os_error()).context("could not read the terminal");
            }
            // Constructed before the changes, so a failure half-way still
            // restores what did take.
            let guard = Self { termios, flags };

            // Raw, so keystrokes reach the child unbuffered and unechoed: the
            // pty's own line discipline is what echoes them, which is how
            // `sudo` turning ECHO off still hides a password. It also turns
            // ISIG off here, so Ctrl-C is forwarded as a byte and the *child's*
            // line discipline raises SIGINT — the signal reaches the process
            // the developer is looking at.
            let mut raw = termios;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                return Err(std::io::Error::last_os_error()).context("could not set the terminal");
            }
            if libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(std::io::Error::last_os_error()).context("could not set the terminal");
            }
            Ok(guard)
        }
    }
}

/// A descriptor `AsyncFd` can watch without owning.
///
/// fd 0 belongs to the shell. Wrapping it in anything that closes on drop would
/// close the developer's terminal out from under riabuild.
struct Borrowed(RawFd);

impl AsRawFd for Borrowed {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

/// The size to give the child: the developer's terminal, less the indent.
fn winsize() -> libc::winsize {
    // SAFETY: a zeroed `winsize` is valid, and the ioctl writes into a local.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ as _, &mut size) };
    if size.ws_row == 0 {
        size.ws_row = 24;
    }
    size.ws_col = child_columns(if size.ws_col == 0 { 80 } else { size.ws_col });
    size
}

/// `openpty`, as an owned pair.
fn open(size: &libc::winsize) -> Result<(OwnedFd, OwnedFd)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    // SAFETY: both out-parameters are locals, and the size is borrowed for the
    // duration of the call.
    let made = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            size,
        )
    };
    if made != 0 {
        return Err(std::io::Error::last_os_error()).context("could not open a pseudo-terminal");
    }
    // `AsyncFd` requires it, and the master is riabuild's own descriptor.
    // SAFETY: `openpty` succeeded, so both are open descriptors this function
    // is the sole owner of.
    unsafe {
        libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
        Ok((OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)))
    }
}

fn stdio(fd: &OwnedFd) -> Result<std::process::Stdio> {
    Ok(std::process::Stdio::from(
        fd.try_clone().context("could not duplicate the terminal")?,
    ))
}

/// Both directions, until the child exits and its output has been drained.
async fn pump(child: &mut Child, master: OwnedFd, theme: Theme) -> Result<i32> {
    let master = AsyncFd::new(master).context("could not watch the pseudo-terminal")?;
    let input =
        AsyncFd::new(Borrowed(libc::STDIN_FILENO)).context("could not watch the terminal")?;
    let mut resized = signal(SignalKind::window_change()).context("could not watch for resizes")?;

    let mut filter = Subdue::new();
    let mut painter = Painter::new(theme);

    let code = loop {
        tokio::select! {
            ready = master.readable() => {
                let mut guard = ready.context("could not watch the pseudo-terminal")?;
                match guard.try_io(|fd| read(fd.as_raw_fd())) {
                    Ok(Ok(bytes)) if bytes.is_empty() => {}
                    Ok(Ok(bytes)) => show(&mut filter, &mut painter, &bytes),
                    // A read on the master once the child has closed the slave
                    // is EIO on Linux and zero bytes on macOS. Both are EOF,
                    // and the exit status is what the loop is waiting for.
                    Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => {}
                    Ok(Err(error)) => {
                        return Err(error).context("could not read from the pseudo-terminal");
                    }
                    Err(_would_block) => {}
                }
            }

            ready = input.readable() => {
                let mut guard = ready.context("could not watch the terminal")?;
                let typed = match guard.try_io(|fd| read(fd.as_raw_fd())) {
                    Ok(Ok(bytes)) => bytes,
                    // The developer's stdin ended, or could not be read. The
                    // child keeps running; it simply gets no more input.
                    Ok(Err(_)) | Err(_) => Vec::new(),
                };
                if !typed.is_empty() {
                    // Forwarded verbatim: unbuffered, uninspected, retained
                    // nowhere. The filter is for the output direction only, and
                    // what the developer types here is a password often enough
                    // that it matters this path holds none of it.
                    forward(&master, &typed).await;
                }
            }

            _ = resized.recv() => {
                let size = winsize();
                // SAFETY: the master is open for the whole of this loop.
                unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) };
            }

            status = child.wait() => {
                break status.context("could not wait for the child")?.code().unwrap_or(1);
            }
        }
    };

    // Whatever the child wrote just before exiting is still in the buffer.
    let _ = tokio::time::timeout(DRAIN, drain(&master, &mut filter, &mut painter)).await;
    if let Some(text) = filter.partial() {
        emit(&painter.line(&text));
    }

    Ok(code)
}

/// Reads what is left in the pty after the child has gone.
async fn drain(master: &AsyncFd<OwnedFd>, filter: &mut Subdue, painter: &mut Painter) {
    loop {
        let Ok(mut guard) = master.readable().await else {
            return;
        };
        match guard.try_io(|fd| read(fd.as_raw_fd())) {
            Ok(Ok(bytes)) if bytes.is_empty() => return,
            Ok(Ok(bytes)) => show(filter, painter, &bytes),
            Ok(Err(_)) => return,
            Err(_would_block) => continue,
        }
    }
}

/// Runs one read's worth of bytes through the filter and paints the result.
fn show(filter: &mut Subdue, painter: &mut Painter, bytes: &[u8]) {
    let mut out = String::new();
    for line in filter.feed(bytes) {
        out.push_str(&painter.line(&line));
    }
    if let Some(text) = filter.partial() {
        out.push_str(&painter.partial(&text));
    }
    emit(&out);
}

fn emit(text: &str) {
    if text.is_empty() {
        return;
    }
    use std::io::Write;
    // Raw mode is on, so a bare `\n` moves down without returning to column
    // zero. Every line the painter produces starts with `\r`, which is what
    // puts it back.
    print!("{text}");
    let _ = std::io::stdout().flush();
}

/// Writes every byte to the master, waiting for room rather than dropping any.
///
/// A keystroke dropped because the pty's input buffer was momentarily full is a
/// character missing from a password, and nothing would say so.
async fn forward(master: &AsyncFd<OwnedFd>, bytes: &[u8]) {
    let mut sent = 0;
    while sent < bytes.len() {
        let Ok(mut guard) = master.writable().await else {
            return;
        };
        match guard.try_io(|fd| write(fd.as_raw_fd(), &bytes[sent..])) {
            Ok(Ok(0)) | Ok(Err(_)) => return,
            Ok(Ok(wrote)) => sent += wrote,
            Err(_would_block) => continue,
        }
    }
}

fn read(fd: RawFd) -> std::io::Result<Vec<u8>> {
    let mut buffer = [0u8; 4096];
    // SAFETY: the buffer is a local of exactly the length passed.
    let got = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if got < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(buffer[..got as usize].to_vec())
}

fn write(fd: RawFd, bytes: &[u8]) -> std::io::Result<usize> {
    // SAFETY: the slice is borrowed for the duration of the call.
    let wrote = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    if wrote < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(wrote as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Depth;

    #[test]
    fn a_line_is_indented_to_note_depth_and_dimmed() {
        let mut painter = Painter::new(Theme::with_depth(Depth::Ansi16));
        assert_eq!(painter.line("Unpacking"), "\r    \x1b[2mUnpacking\x1b[0m\n");
    }

    #[test]
    fn a_plain_theme_still_indents_and_still_ends_the_line() {
        let mut painter = Painter::new(Theme::plain());
        assert_eq!(painter.line("Unpacking"), "\r    Unpacking\n");
    }

    #[test]
    fn a_partial_line_is_written_without_ending_it() {
        let mut painter = Painter::new(Theme::plain());
        assert_eq!(painter.partial("Password: "), "\r    Password: ");
    }

    #[test]
    fn a_redraw_covers_what_the_longer_frame_left_behind() {
        let mut painter = Painter::new(Theme::plain());
        painter.partial("Reading database... 45%");
        assert_eq!(painter.partial("Done"), "\r    Done                   ");
    }

    #[test]
    fn a_line_after_a_partial_covers_it_too() {
        let mut painter = Painter::new(Theme::plain());
        painter.partial("Progress: 100%");
        assert_eq!(painter.line("Done"), "\r    Done          \n");
    }

    #[test]
    fn repainting_the_same_partial_writes_nothing() {
        // The pump repaints after every read. A child that writes a prompt and
        // then waits must not have it reprinted on each wakeup.
        let mut painter = Painter::new(Theme::plain());
        painter.partial("Password: ");
        assert_eq!(painter.partial("Password: "), "");
    }

    #[test]
    fn a_finished_line_stops_covering_for_the_next_one() {
        // `line` clears the open width; otherwise the padding from a long
        // progress bar would be re-applied to every line after it.
        let mut painter = Painter::new(Theme::plain());
        painter.partial("a very long progress line");
        painter.line("short");
        assert_eq!(painter.line("also short"), "\r    also short\n");
    }

    #[test]
    fn the_child_gets_the_terminal_width_less_the_indent() {
        // Otherwise the child wraps at the full width and the indent pushes
        // every wrapped line four columns past the right edge.
        assert_eq!(child_columns(80), 76);
        // Never zero, whatever the terminal claims.
        assert_eq!(child_columns(4), 1);
        assert_eq!(child_columns(0), 1);
    }

    #[test]
    fn a_pty_is_only_offered_when_both_ends_are_a_terminal() {
        // Under `cargo test` they are not, which is the degradation the design
        // asks for: an unattended run takes exactly the path it always did.
        assert!(!available());
    }
}
