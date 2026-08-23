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

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::unix::AsyncFd;
use tokio::process::{Child, Command};
use tokio::signal::unix::{SignalKind, signal};

use super::subdue::Subdue;
use riabuild_theme::Theme;

mod painter;
mod terminal;

use painter::{Painter, emit, show};
use terminal::{Borrowed, Restore, open, stdio, winsize};

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
    let (master, slave) = open(&mut winsize())?;

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

    // For the reason `child.rs` sets it: every path out of this function below
    // the spawn — the terminal that cannot be read, a pump that fails, a
    // cancelled task — drops the child, and a child dropped without this is
    // left running against a master nobody is reading and then left as a
    // zombie. A subdued child is `sudo apt-get`, so the one left behind holds
    // the package lock the next run needs.
    command.kill_on_drop(true);

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

/// Whether a read from the master says the child has closed the last copy of
/// the slave.
///
/// Zero bytes on macOS, `EIO` on Linux; both are end of file. Ignoring one is
/// not enough — the arm has to be *disabled*. `AsyncFd::try_io` clears
/// readiness only on `WouldBlock`, so a master left watched after end of file
/// is ready on every pass and spins the pump at 100% CPU until `child.wait()`
/// resolves — which is for ever if the child closed its own copies of the slave
/// and kept running, and on a current-thread runtime takes the reactor with it.
/// The input arm has carried the same guard, for the same reason, since it was
/// written.
fn closed(outcome: &std::io::Result<Vec<u8>>) -> bool {
    match outcome {
        Ok(bytes) => bytes.is_empty(),
        Err(error) => error.raw_os_error() == Some(libc::EIO),
    }
}

/// Both directions, until the child exits and its output has been drained.
async fn pump(child: &mut Child, master: OwnedFd, theme: Theme) -> Result<i32> {
    let master = AsyncFd::new(master).context("could not watch the pseudo-terminal")?;
    let input =
        AsyncFd::new(Borrowed(libc::STDIN_FILENO)).context("could not watch the terminal")?;
    let mut resized = signal(SignalKind::window_change()).context("could not watch for resizes")?;

    let mut filter = Subdue::new();
    let mut painter = Painter::new(theme);
    // Whether the developer's stdin is still worth watching.
    let mut listening = true;
    // And whether the child's end of the pty is. Both flags exist for the same
    // reason: `try_io` clears readiness only on `WouldBlock`, so an arm left
    // enabled after end of file is ready on every pass.
    let mut watching = true;

    let code = loop {
        tokio::select! {
            ready = master.readable(), if watching => {
                let mut guard = ready.context("could not watch the pseudo-terminal")?;
                match guard.try_io(|fd| read(fd.as_raw_fd())) {
                    // The exit status is what the loop is waiting for from
                    // here on; the drain below picks up anything still in the
                    // buffer.
                    Ok(outcome) if closed(&outcome) => watching = false,
                    Ok(Ok(bytes)) => show(&mut filter, &mut painter, &bytes),
                    Ok(Err(error)) => {
                        return Err(error).context("could not read from the pseudo-terminal");
                    }
                    Err(_would_block) => {}
                }
            }

            ready = input.readable(), if listening => {
                let mut guard = ready.context("could not watch the terminal")?;
                let typed = match guard.try_io(|fd| read(fd.as_raw_fd())) {
                    // A terminal at end of file stays readable for ever. Left
                    // watched, this arm would be ready on every pass and spin
                    // the loop hot for the whole of the child's life — on a
                    // current-thread runtime, at the exact moment a developer
                    // is watching a package install.
                    Ok(Ok(bytes)) if bytes.is_empty() => {
                        listening = false;
                        Vec::new()
                    }
                    Ok(Ok(bytes)) => bytes,
                    // Unreadable for any other reason. The child keeps running;
                    // it simply gets no more input.
                    Ok(Err(_)) => {
                        listening = false;
                        Vec::new()
                    }
                    Err(_) => Vec::new(),
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

    #[test]
    fn a_master_at_end_of_file_stops_being_watched() {
        // Both spellings of the same event: macOS reads zero bytes where Linux
        // fails with EIO. Either one left on the watched list is a pump that
        // spins at 100% CPU for as long as the child lives, so both have to
        // end the arm rather than merely be ignored.
        assert!(closed(&Ok(Vec::new())), "a zero-byte read is end of file");
        assert!(
            closed(&Err(std::io::Error::from_raw_os_error(libc::EIO))),
            "EIO is end of file"
        );
    }
    #[test]
    fn a_master_with_something_to_say_is_still_watched() {
        // And a failure that is not end of file is not silently turned into
        // one: it is the error the pump reports.
        assert!(!closed(&Ok(b"Unpacking".to_vec())));
        assert!(!closed(&Err(std::io::Error::from_raw_os_error(
            libc::EBADF
        ))));
    }
    #[test]
    fn a_pty_is_only_offered_when_both_ends_are_a_terminal() {
        // Under `cargo test` they are not, which is the degradation the design
        // asks for: an unattended run takes exactly the path it always did.
        // Ignored when the suite is run under `script` for the test below.
        if std::env::var_os("RIABUILD_PTY_TEST").is_none() {
            assert!(!available());
        }
    }
    /// The terminal's `c_lflag`, which carries `ECHO` and `ICANON` — the two
    /// bits a developer notices the loss of.
    #[cfg(any(test, feature = "testing"))]
    fn lflag() -> libc::tcflag_t {
        // SAFETY: writes into a local.
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            libc::tcgetattr(libc::STDIN_FILENO, &mut termios);
            termios.c_lflag
        }
    }
    /// End to end: a real `openpty`, a real `setsid`, a real child.
    ///
    /// Ignored because it needs a controlling terminal, which `cargo test` does
    /// not have — that being the degradation the rest of the design rests on.
    /// To run it, give the test binary one:
    ///
    /// ```sh
    /// RIABUILD_PTY_TEST=1 script -qec "cargo test -- --ignored pty" /dev/null
    /// ```
    ///
    /// Add `--nocapture` to watch what actually reaches the terminal. Expect a
    /// leading `^@` on the first line and do not go looking for it in this
    /// crate: `script` puts a stray NUL on the test's stdin, the pump forwards
    /// it verbatim the way it forwards every keystroke, and the child's own
    /// line discipline echoes it back as those two characters with `ECHOCTL`.
    /// A developer's terminal does not do this; a developer pressing Ctrl-@
    /// would, and printing `^@` is what a terminal does then too.
    #[tokio::test]
    #[ignore = "needs a controlling terminal; see the doc comment"]
    async fn a_real_child_runs_under_a_pty_and_gives_the_terminal_back() {
        assert!(available(), "run this under `script`, per the doc comment");

        let before = lflag();
        let mut command = Command::new("/bin/sh");
        // Colour, a rewrite, and a window-title attempt — none of which should
        // reach the terminal — and an exit code that has to survive the pump.
        command.arg("-c").arg(
            "printf '\\033]0;stolen\\007\\033[32mworking\\033[0m\\r\\n'; \
             printf 'Progress: 20%%\\rProgress: 100%%\\n'; exit 7",
        );

        let code = run(command, Theme::plain(), "sh").await.expect("pty run");
        assert_eq!(code, 7, "the child's exit status reaches the caller");
        assert_eq!(lflag(), before, "the terminal is put back out of raw mode");

        // SAFETY: reads a flag from fd 0.
        let flags = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL) };
        assert_eq!(
            flags & libc::O_NONBLOCK,
            0,
            "fd 0 is put back to blocking, or the shell inherits a broken stdin"
        );
    }
}
