//! Reading one secret from the terminal, with the terminal's echo turned off.
//!
//! Everything else in `ui/` reads stdin and writes stdout. This does neither,
//! and both departures are load-bearing:
//!
//! - **`/dev/tty`, not stdin/stdout.** The only caller is `riabuild internal
//!   askpass`, which `ssh` starts with its stdout attached to a pipe `ssh`
//!   reads the answer from. A prompt written to stdout there would be read
//!   *as the password*, and a read from stdin would take whatever `ssh` left
//!   on it. `/dev/tty` is the developer's terminal regardless of what the
//!   parent process redirected, which is exactly the property needed.
//! - **Echo off.** `Ui::ask` is a plain `read_line`, so anything typed at it
//!   is echoed to the screen and stays in scrollback. That was the first of
//!   the two reasons `authorise` never prompted for a password itself, and
//!   this file is what answers it.
//!
//! The terminal mode is restored by a guard rather than by the happy path, so
//! a read that fails, or a caller that returns early, cannot leave a
//! developer's shell with echo permanently off.

use crate::Failure;
use anyhow::Result;
use std::io::{BufRead, BufReader, Read, Write};

/// The prompt-and-read itself, over anything readable and writable, so the
/// rules are testable without a terminal to attach to.
///
/// The answer keeps every character the developer typed except the line
/// ending: a password may legitimately begin or end with a space, so this
/// strips `\n` and a preceding `\r` and nothing else. `trim()` here would
/// silently sign a developer in with a different password than the one they
/// have, and the failure would look like a wrong password rather than like
/// riabuild editing it.
pub fn prompt_and_read<R: Read, W: Write>(
    reader: R,
    mut writer: W,
    prompt: &str,
) -> std::io::Result<String> {
    write!(writer, "{prompt}")?;
    writer.flush()?;

    let mut line = String::new();
    let read = BufReader::new(reader).read_line(&mut line)?;
    // Echo is off, so the newline the developer typed was never shown. Ending
    // the line here is what stops the next thing printed from landing on the
    // end of the prompt.
    writeln!(writer)?;
    if read == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "the terminal closed before an answer was typed",
        ));
    }

    let line = line.strip_suffix('\n').unwrap_or(&line);
    Ok(line.strip_suffix('\r').unwrap_or(line).to_string())
}

/// Asks for a secret on the controlling terminal, with echo off.
///
/// Fails rather than returning an empty answer when there is no terminal:
/// this is the `ask_required` case, not the `ask` one — there is no default a
/// password could fall back to, and an unattended run must say so instead of
/// handing `ssh` an empty string and reporting a wrong password.
#[cfg(unix)]
pub fn ask_secret(prompt: &str) -> Result<String> {
    use std::os::unix::io::AsRawFd;

    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|error| {
            Failure::new(
                "asking you for that server's password",
                "Run `riabuild remote` from a terminal, where riabuild can ask for it.",
            )
            .detail(format!("could not open /dev/tty: {error}"))
        })?;

    let _echo_off = EchoOff::on(tty.as_raw_fd()).map_err(|error| {
        Failure::new(
            "asking you for that server's password",
            "Run `riabuild remote` from a terminal, where riabuild can ask for it.",
        )
        .detail(format!("could not turn off terminal echo: {error}"))
    })?;

    prompt_and_read(&tty, &tty, prompt).map_err(|error| {
        Failure::new(
            "asking you for that server's password",
            "Run `riabuild remote` again and type the password when it asks.",
        )
        .detail(error.to_string())
        .into()
    })
}

#[cfg(not(unix))]
pub fn ask_secret(_prompt: &str) -> Result<String> {
    Err(Failure::new(
        "asking you for that server's password",
        "riabuild's remote mode runs on macOS and Linux only.",
    )
    .into())
}

/// Turns off terminal echo for as long as it is alive.
///
/// The restore is in `Drop` and not at the end of `ask_secret` because every
/// path out of that function — a read error, a closed terminal, an early
/// `?` — has to put the terminal back. A developer whose shell is left with
/// echo off has no way to know why, and typing `stty sane` blind is not a
/// thing riabuild may ask of anyone.
#[cfg(unix)]
struct EchoOff {
    fd: std::os::unix::io::RawFd,
    original: libc::termios,
}

#[cfg(unix)]
impl EchoOff {
    fn on(fd: std::os::unix::io::RawFd) -> std::io::Result<Self> {
        // SAFETY: `fd` is an open file descriptor for the terminal, held by
        // the caller for longer than this guard, and `termios` is a plain
        // struct the call fills in.
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut quiet = original;
        quiet.c_lflag &= !libc::ECHO;
        // `TCSAFLUSH` rather than `TCSANOW`: it discards anything already
        // typed but not yet read, so a keystroke that arrived before the
        // prompt appeared cannot be echoed by the old mode and then land in
        // the password.
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &quiet) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { fd, original })
    }
}

#[cfg(unix)]
impl Drop for EchoOff {
    fn drop(&mut self) {
        // Nothing useful can be done if this fails, and a panic in `Drop`
        // during unwinding aborts the process.
        unsafe { libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.original) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn the_answer_keeps_every_character_except_the_line_ending() {
        // A password may begin or end with a space. `trim()` here would sign
        // the developer in with a password they do not have, and the server
        // would report it as wrong rather than as edited.
        let mut written = Vec::new();
        let answer = prompt_and_read(
            Cursor::new(b"  hunter2  \n".to_vec()),
            &mut written,
            "Password: ",
        )
        .expect("reads");
        assert_eq!(answer, "  hunter2  ");
    }

    #[test]
    fn a_windows_line_ending_is_not_part_of_the_password() {
        let mut written = Vec::new();
        let answer =
            prompt_and_read(Cursor::new(b"hunter2\r\n".to_vec()), &mut written, "").expect("reads");
        assert_eq!(answer, "hunter2");
    }

    #[test]
    fn the_prompt_goes_to_the_writer_and_the_answer_does_not() {
        // The writer is the terminal; stdout is the channel `ssh` reads the
        // answer from. A prompt written there would be read as the password.
        let mut written = Vec::new();
        prompt_and_read(
            Cursor::new(b"hunter2\n".to_vec()),
            &mut written,
            "ada@box's password: ",
        )
        .expect("reads");
        let shown = String::from_utf8(written).expect("utf8");
        assert!(shown.starts_with("ada@box's password: "), "{shown}");
        assert!(
            !shown.contains("hunter2"),
            "echo is off, so the answer must never be written back: {shown}"
        );
        assert!(
            shown.ends_with('\n'),
            "the newline the developer typed was not echoed, so this has to \
             supply one: {shown:?}"
        );
    }

    #[test]
    fn a_terminal_that_closes_before_an_answer_is_an_error_not_an_empty_password() {
        // ^D at the prompt. An empty string handed to `ssh` reads as a wrong
        // password, which sends the developer looking for the wrong problem.
        let mut written = Vec::new();
        let error = prompt_and_read(Cursor::new(Vec::new()), &mut written, "Password: ")
            .expect_err("EOF is not an answer");
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}
