//! Telling a broken binary from a machine that could not spawn anything.
//!
//! Every `run` that fails to start a child comes back as the same shape — an
//! `anyhow` chain headed ``could not start `the program` ``, wrapping the `io::Error`
//! the spawn itself produced. That single shape covers two situations a task has
//! to treat as opposites:
//!
//! - **The program cannot be executed.** `EACCES` on a file whose mode is not
//!   executable, `ENOEXEC` on one that is not a program at all, `ENOENT` on a
//!   shebang naming an interpreter that is not there. Every one of these is a
//!   fact about *that path*, it will say the same thing on every run until
//!   something repairs it, and a task that installed the tool is the thing that
//!   can.
//! - **The machine could not spawn anything.** `EAGAIN` under a process limit,
//!   `ENOMEM` under memory pressure. These say nothing whatever about the file,
//!   and `toolchain`'s `a_node_that_will_not_start_is_not_evidence_that_it_is_missing`
//!   is the test that pins what reading them as "not installed" cost: a ~130 MB
//!   download that swapped out the Node a co-tenant's live session was
//!   executing from.
//!
//! So the distinction is drawn here, once, rather than by each task guessing
//! from a message string.

/// Whether a failed spawn means *this program* cannot be executed.
///
/// `false` for every error that is about the machine rather than the path,
/// which includes a timeout — `capture` raises that with `anyhow::bail!` and no
/// `io::Error` under it, so it cannot be mistaken for a broken file.
///
/// The check is by `ErrorKind` where one exists and by errno where one does
/// not: `ENOEXEC` has no stable `ErrorKind` (it maps to the unstable
/// `Uncategorized`), and 8 is its value on both Linux and macOS.
pub fn cannot_execute(error: &anyhow::Error) -> bool {
    /// `ENOEXEC` — the file is there and is not something the kernel can run.
    const ENOEXEC: i32 = 8;

    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
            ) || io.raw_os_error() == Some(ENOEXEC)
        })
}

#[cfg(test)]
mod tests {
    use super::cannot_execute;
    use anyhow::Context;

    /// The shape `RealRunner::start` produces, so these test what tasks see
    /// rather than what this module would like to be handed.
    fn spawn_failure(io: std::io::Error) -> anyhow::Error {
        Err::<(), _>(io)
            .with_context(|| "could not start `claude`".to_string())
            .unwrap_err()
    }

    #[test]
    fn a_binary_that_is_not_executable_is_about_the_binary() {
        // The case this exists for. An interrupted `npm install -g
        // @anthropic-ai/claude-code` leaves a 0644 placeholder at
        // `bin/claude.exe`, and `claude --version` then fails `EACCES` on every
        // run for ever.
        assert!(cannot_execute(&spawn_failure(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        ))));
    }

    #[test]
    fn a_file_that_is_not_a_program_is_about_the_binary() {
        assert!(cannot_execute(&spawn_failure(
            std::io::Error::from_raw_os_error(8)
        )));
    }

    #[test]
    fn a_missing_interpreter_is_about_the_binary() {
        // Reached only past an existence test, so `ENOENT` here is a shebang
        // naming something that is not there rather than the tool being absent.
        assert!(cannot_execute(&spawn_failure(std::io::Error::from(
            std::io::ErrorKind::NotFound
        ))));
    }

    #[test]
    fn a_machine_that_cannot_spawn_is_not_about_the_binary() {
        // `EAGAIN`. Reading this as a broken install is what reinstalled a
        // perfectly good Node out from under a colleague.
        assert!(!cannot_execute(&spawn_failure(
            std::io::Error::from_raw_os_error(11)
        )));
        // `ENOMEM`.
        assert!(!cannot_execute(&spawn_failure(
            std::io::Error::from_raw_os_error(12)
        )));
    }

    #[test]
    fn a_timeout_is_not_about_the_binary() {
        // `capture` raises this with `anyhow::bail!`, so there is no
        // `io::Error` in the chain to misread.
        assert!(!cannot_execute(&anyhow::anyhow!(
            "`claude` did not finish within 1800 seconds"
        )));
    }

    #[test]
    fn a_double_that_names_no_errno_is_not_about_the_binary() {
        // `CannotSpawn`, the double `toolchain` uses, is a bare
        // `anyhow!("could not start ...")`. It must keep meaning what it
        // meant — the machine, not the file — or this change would quietly
        // invert the test that guards the co-tenant case.
        assert!(!cannot_execute(&anyhow::anyhow!(
            "could not start `node`: Resource temporarily unavailable (os error 11)"
        )));
    }
}
