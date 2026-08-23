//! The pseudo-terminal itself, and the developer's own terminal around it.
//!
//! Opening the pair, handing the slave to a child, and — the part that is not
//! housekeeping — putting the shell's terminal back the way it was found.
//! Raw mode and `O_NONBLOCK` are changes to a file description the shell
//! shares, so a terminal left raw outlives the process that did it.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use anyhow::{Context, Result};

use super::painter::child_columns;

/// The developer's terminal, put back the way it was found.
///
/// Raw mode and `O_NONBLOCK` are changes to a file description the shell
/// shares. Restoring them on an early return is not tidiness: a terminal left
/// raw with no echo outlives the process that did it, and the developer's next
/// command is typed into a shell that shows them nothing.
pub(super) struct Restore {
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
    pub(super) fn take() -> Result<Self> {
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
pub(super) struct Borrowed(pub(super) RawFd);

impl AsRawFd for Borrowed {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

/// The size to give the child: the developer's terminal, less the indent.
pub(super) fn winsize() -> libc::winsize {
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
///
/// The size is taken by `&mut` and the null termios is `null_mut` because
/// Apple's libc declares both of those parameters `*mut` where Linux's declares
/// them `*const`. A `*mut` coerces to a `*const`, so passing the mutable form
/// is the one spelling that compiles on both — and this is `runner/`, which is
/// not a file allowed to branch on the operating system.
pub(super) fn open(size: &mut libc::winsize) -> Result<(OwnedFd, OwnedFd)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    // SAFETY: both out-parameters are locals, and the size is borrowed for the
    // duration of the call.
    let made = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
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

pub(super) fn stdio(fd: &OwnedFd) -> Result<std::process::Stdio> {
    Ok(std::process::Stdio::from(
        fd.try_clone().context("could not duplicate the terminal")?,
    ))
}
