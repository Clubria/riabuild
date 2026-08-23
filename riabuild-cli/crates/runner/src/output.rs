//! What a captured child hands back.
//!
//! Two types rather than one because the clipboard channel moves PNGs: the
//! lossy `String` that is right for every `--version` probe in riabuild is a
//! corrupt image for the one caller that is not probing anything.

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// stdout with trailing newline removed — what a `--version` check wants.
    pub fn trimmed(&self) -> &str {
        self.stdout.trim()
    }
}

/// Subprocess output whose stdout is not assumed to be text.
///
/// `CommandOutput` exists for the `--version` and status checks that make up
/// most of riabuild, and its lossy `String` conversion is right for those. The
/// clipboard channel moves PNGs, where a single replacement character is a
/// corrupt image, so it reads through here instead. stderr stays a `String`:
/// it is diagnostics, it is always text, and every caller puts it in a message.
#[derive(Debug, Clone)]
pub struct BytesOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl BytesOutput {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

#[cfg(test)]
mod bytes_tests {
    use crate::*;

    /// A PNG is not valid UTF-8. Read through `run`, its bytes come back
    /// mangled into replacement characters; `run_bytes` is what makes the
    /// clipboard channel possible at all.
    #[tokio::test]
    async fn binary_stdout_survives_the_runner() {
        // PNG magic, then a byte that is illegal as UTF-8 on its own.
        let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0xFF];
        let runner = FakeRunner::new().with_bytes("xclip -o", 0, &png, "");

        let out = runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();

        assert!(out.ok());
        assert_eq!(out.stdout, png);
    }

    /// The bug this whole method exists to avoid, pinned against the real
    /// runner so nobody "simplifies" the clipboard backends back onto `run`.
    #[tokio::test]
    async fn the_same_bytes_through_run_would_have_been_corrupted() {
        let png = [0x89u8, b'P', b'N', b'G', 0xFF];
        let emit = ["-c", r"printf '\211PNG\377'"];

        let lossy = RealRunner
            .run("sh", &emit, &RunOptions::default())
            .await
            .unwrap();
        let raw = RealRunner
            .run_bytes("sh", &emit, &RunOptions::default())
            .await
            .unwrap();

        assert_eq!(raw.stdout, png);
        assert_ne!(lossy.stdout.as_bytes(), png);
        assert!(
            lossy.stdout.contains('\u{FFFD}'),
            "expected replacement characters, got {:?}",
            lossy.stdout
        );
    }

    #[tokio::test]
    async fn an_unstubbed_command_fails_the_same_way_as_run() {
        let runner = FakeRunner::new();
        let out = runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(out.code, Some(127));
        assert!(out.stdout.is_empty());
        assert!(out.stderr.contains("no stub"), "{}", out.stderr);
    }

    #[tokio::test]
    async fn bytes_calls_are_recorded_like_every_other_call() {
        let runner = FakeRunner::new().with_bytes("xclip -o", 0, b"hi", "");
        runner
            .run_bytes("xclip", &["-o"], &RunOptions::default())
            .await
            .unwrap();
        assert_eq!(runner.calls(), vec!["xclip -o".to_string()]);
    }

    /// The real runner is exercised through a shell builtin every supported
    /// platform has, so this stays a unit test rather than a fixture.
    #[tokio::test]
    async fn the_real_runner_returns_raw_bytes() {
        let out = RealRunner
            .run_bytes(
                "sh",
                &["-c", r"printf '\211PNG\377'"],
                &RunOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout, [0x89u8, b'P', b'N', b'G', 0xFF]);
    }
}
