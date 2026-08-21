//! Whether this machine has a Secret Service that actually answers.
//!
//! One question, one place. `keyring_answers` is the only thing in riabuild
//! that decides it, and `runner.which("secret-tool")` is not an answer to it —
//! all three of `for_platform`, `for_password` and `for_account` ask this
//! function, and a fourth call site that asks `which` instead will look
//! correct, pass CI, and fail on a server. It sits beside the two stores rather
//! than inside either because it is asked *before* one is chosen.

use super::SERVICE;
use riabuild_runner::{CommandRunner, RunOptions};
use std::time::Duration;

/// The account [`keyring_answers`] looks up. Deliberately one nothing ever
/// stores, so the probe's answer is "does the service reply" and never "is
/// this developer signed in" — a machine with a perfectly good keyring and no
/// token yet must still be recognised as having one.
const PROBE_ACCOUNT: &str = "keyring-probe";

/// How long the probe waits for a Secret Service to answer before concluding
/// there is not one.
///
/// Without a bound there is no answer at all. `secret-tool lookup` is a D-Bus
/// method call, and libsecret sets no deadline of its own: a bus with a locked
/// collection and no prompter to unlock it leaves the call outstanding for
/// ever. That is not an exotic machine — a headless Linux box with
/// `secret-tool` installed and nothing listening is, in `CLAUDE.md`'s words,
/// an ordinary machine to run a provisioner on. This probe is the *first*
/// thing riabuild does on Linux (`for_platform`, `for_password` and
/// `for_account` all ask it), so an unbounded one hangs riabuild before it has
/// printed a line, with no output and no error to send anyone.
///
/// Three seconds, bounded from both ends:
///
/// - **Generous for a real keyring.** A Secret Service already on the bus
///   answers a `lookup` in single-digit milliseconds. The slow legitimate case
///   is the bus *autostarting* `gnome-keyring-daemon` on the first call — a
///   process spawn and a collection open, comfortably inside a second on a
///   machine that has only just booted. Three seconds is two orders of
///   magnitude of headroom over the normal case and several times the slow
///   one.
/// - **Short for a dead bus**, because it is paid over and over. Every
///   riabuild invocation pays it once, and `remote::askpass::store` builds two
///   stores — so two probes run *inside each SSH authentication attempt*, and
///   one `riabuild remote` opens around ten connections. At three seconds the
///   worst case is tens of seconds of a slower run; at thirty it is minutes of
///   silence, which is the bug rather than the fix.
///
/// Elapsing is read as "no keyring", the same safe direction the rest of this
/// probe already fails in: the token goes to a 0600 file and `describe()` says
/// so, rather than riabuild stopping.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Whether this machine has a Secret Service that actually answers.
///
/// This is the question `runner.which("secret-tool").is_some()` was standing
/// in for, and getting wrong. libsecret is a **client** for a D-Bus Secret
/// Service; `secret-tool` being on `PATH` says nothing about whether anything
/// is listening. `libsecret-tools` arrives as a transitive dependency of
/// plenty of packages, so the binary is present on servers that have no
/// session bus at all — and riabuild would then pick the keyring, run the
/// whole device-code flow, and only discover it had nowhere to put the token
/// *after* the developer had approved the machine in a browser.
///
/// A `lookup` is the probe because it is read-only: it cannot create, unlock,
/// or overwrite anything. Its exit status alone cannot answer the question —
/// a miss and a dead service both exit non-zero — but **stderr** can, and this
/// was measured rather than assumed, against a real Secret Service on the bus
/// and against both ways of not having one:
///
/// | machine | exit | stderr |
/// |---|---|---|
/// | service on the bus, item absent | 1 | *empty* |
/// | no session bus at all | 1 | `Cannot autolaunch D-Bus without X11 $DISPLAY` |
/// | bus, but no keyring daemon | 1 | `The name org.freedesktop.secrets was not provided…` |
///
/// So: a diagnostic on stderr means the call did not complete, and the rule
/// below reads that rather than matching any of those messages as text, which
/// would be one libsecret release or one non-English locale from breaking.
///
/// It fails in the safe direction. If some future libsecret did print to
/// stderr on an ordinary miss, riabuild would keep the token in a 0600 file
/// instead of the keyring — a visible downgrade rather than a broken machine,
/// and `provision.rs` prints `describe()`, so the developer is told where the
/// token went.
pub(crate) async fn keyring_answers(runner: &dyn CommandRunner) -> bool {
    if runner.which("secret-tool").is_none() {
        return false;
    }
    // The bound is stated at both layers, and the two are one deadline rather
    // than two budgets.
    //
    // `RunOptions::timeout` is the bound on the **child**. It is what makes
    // the wedged `secret-tool` be killed and reaped rather than left holding a
    // D-Bus connection for the rest of the session, and it is the field's own
    // argument for existing: a bound belongs at the layer every subprocess
    // already goes through.
    //
    // The wrapper is the bound on **this function**, which returns a `bool`
    // that three call sites treat as a cheap question and none of them can
    // retry. It holds for any `CommandRunner`, not only the one that honours
    // the field — and it is the half a test can pin, because a runner that
    // never returns is the exact shape of this failure and no assertion about
    // an options struct would have caught it. Dropping the expired future
    // drops the child with it (`RealRunner::start` sets `kill_on_drop`), so
    // whichever fires first, nothing is left running.
    let options = RunOptions {
        timeout: Some(PROBE_TIMEOUT),
        ..Default::default()
    };
    let probe = runner.run(
        "secret-tool",
        &["lookup", "service", SERVICE, "account", PROBE_ACCOUNT],
        &options,
    );
    let Ok(Ok(output)) = tokio::time::timeout(PROBE_TIMEOUT, probe).await else {
        return false;
    };
    output.ok() || output.stderr.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::ACCOUNT;
    use anyhow::Result;
    use async_trait::async_trait;
    use riabuild_runner::{Decoration, Delegating, FakeRunner};
    use std::sync::Arc;

    /// A runner that starts the probe and never comes back — the wedged D-Bus
    /// call, with no bus needed to reproduce it.
    ///
    /// A `Decoration` rather than a hand-written `CommandRunner` so it stays
    /// one method: `Delegating` forwards the other six, including the
    /// synchronous `which`, which the probe asks first and which must still
    /// answer for the test to reach the hang at all.
    struct NeverAnswers;

    #[async_trait]
    impl Decoration for NeverAnswers {
        async fn before(
            &self,
            _program: &str,
            _args: &[&str],
            _options: &RunOptions,
        ) -> Result<()> {
            std::future::pending().await
        }
    }

    // `keyring_answers` — the probe that replaced `which("secret-tool")`.
    //
    // Every row below is a state observed against a real `secret-tool`: a
    // mock Secret Service on a private bus for the healthy miss, and a
    // headless box for the two failures. The table on `keyring_answers`
    // records the measurements; these pin the decision they imply.

    #[tokio::test]
    async fn a_healthy_keyring_with_no_token_yet_still_counts_as_a_keyring() {
        // The load-bearing one. A real Secret Service answering "I do not hold
        // that item" exits non-zero and prints *nothing*, so exit status alone
        // cannot tell this apart from a dead service — and if this were read as
        // "no keyring", every fresh laptop would quietly get a file instead of
        // the keychain.
        let runner = FakeRunner::new().with("secret-tool lookup", 1, "", "");
        assert!(keyring_answers(&runner).await);
    }

    #[tokio::test]
    async fn an_existing_item_counts_as_a_keyring() {
        let runner = FakeRunner::new().with("secret-tool lookup", 0, "rb_token\n", "");
        assert!(keyring_answers(&runner).await);
    }

    #[tokio::test]
    async fn a_box_with_no_dbus_session_has_no_keyring() {
        // The reported failure, verbatim. `secret-tool` is installed here —
        // which is exactly why `which` was the wrong question.
        let runner = FakeRunner::new().with(
            "secret-tool lookup",
            1,
            "",
            "secret-tool: Cannot autolaunch D-Bus without X11 $DISPLAY",
        );
        assert!(runner.which("secret-tool").is_some(), "the binary is there");
        assert!(!keyring_answers(&runner).await);
    }

    #[tokio::test]
    async fn a_bus_with_no_keyring_daemon_has_no_keyring() {
        // The second way to have no Secret Service: a session bus exists and
        // nothing has claimed `org.freedesktop.secrets` on it. Common in
        // containers and on minimal installs.
        let runner = FakeRunner::new().with(
            "secret-tool lookup",
            1,
            "",
            "secret-tool: The name org.freedesktop.secrets was not provided by any .service files",
        );
        assert!(!keyring_answers(&runner).await);
    }

    #[tokio::test]
    async fn a_missing_secret_tool_has_no_keyring() {
        assert!(!keyring_answers(&FakeRunner::new()).await);
    }

    #[tokio::test]
    async fn the_probe_reads_an_account_nothing_ever_stores() {
        // The probe must answer "does the service reply", not "is this
        // developer signed in" — otherwise a working keyring holding no token
        // yet would be misread as no keyring at all, on precisely the first
        // run that needs to store one. It must also never look at, let alone
        // disturb, the real session item.
        let runner = FakeRunner::new().with("secret-tool lookup", 1, "", "");
        keyring_answers(&runner).await;
        let calls = runner.calls();
        assert!(
            calls.iter().any(|call| call.contains(PROBE_ACCOUNT)),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call.contains(ACCOUNT)),
            "the probe must not read the real session item: {calls:?}"
        );
        assert!(
            calls.iter().all(|call| !call.contains("store")
                && !call.contains("clear")
                && !call.contains("--unlock")),
            "the probe must be read-only: {calls:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_bus_that_never_answers_makes_the_probe_give_up_rather_than_hang() {
        // I050. `secret-tool lookup` is a D-Bus call with no deadline of its
        // own, so a bus holding a locked collection with no prompter leaves it
        // outstanding for ever. That machine — `secret-tool` installed, no
        // session bus answering — is the ordinary headless Linux box
        // `CLAUDE.md` describes, and the probe is the first thing riabuild
        // does there, as well as running twice inside every SSH
        // authentication attempt `remote::askpass` makes.
        //
        // The runner never returns, which is the only shape that can catch
        // this: `FakeRunner` does not honour `RunOptions::timeout`, so a bound
        // left entirely to the runner would leave `keyring_answers` itself
        // unbounded and this test would hang instead of failing.
        let runner = Delegating::around(
            Arc::new(FakeRunner::new().with("secret-tool lookup", 1, "", "")),
            NeverAnswers,
        );
        // The clock is paused, so the probe's own three seconds pass for free.
        // The outer bound is what turns a regression into a red test: with the
        // probe's timeout removed there is no other timer for the runtime to
        // advance to, so this one fires immediately rather than the job
        // hanging.
        let answered = tokio::time::timeout(Duration::from_secs(600), keyring_answers(&runner))
            .await
            .expect("the probe must answer rather than hang");
        assert!(!answered, "a bus that never answers is not a keyring");
    }
}
