//! What the supervisor says about a channel that will not come up, and when.
//!
//! Three sentences and one predicate. The predicate is here rather than inline
//! in the loop because it is the whole of the decision and the loop around it
//! cannot be unit-tested without an `ssh`; the sentences are here because
//! choosing between them is the same kind of judgement — which of two walls
//! this is, and whether it is about the server at all.
//!
//! [`report`] is where all three land. It never claims riabuild stopped, and it
//! prefers the status bar to the screen, because this runs beside a shell that
//! owns the terminal.

use riabuild_ui::{Failure, StatusBar, Ui};

/// How many consecutive failures, with the channel never once having come up,
/// before the supervisor says out loud that it cannot reach the server.
///
/// Four puts it around half a minute into the backoff schedule — long enough
/// that an ordinary reconnect after a closed lid stays silent, short enough
/// that a channel which is never coming up says so while the developer is still
/// wondering why paste does nothing.
pub(super) const QUIET_FAILURES: u32 = 4;

/// Whether this failure is the one to say out loud.
///
/// A predicate rather than three conditions inline, because it is the whole of
/// the decision and the loop around it cannot be unit-tested without an `ssh`:
/// `supervise` takes an owned `Ui`, so a test cannot hold on to the printer it
/// moved in and read back what was said. Extracted, every branch is reachable.
///
/// `ever_connected`, not "ever carried a request", and the difference is the
/// bug this sentence exists to keep fixed. Those were one flag, and on a link
/// that drops and rebuilds — which is the whole reason the developer is on mosh
/// — a channel nobody happened to paste through carried nothing on any attempt.
/// Four rebuilds later riabuild told them it could not reach a server it had
/// reached every single time. What proves a connection came up is the pump's
/// keepalive, which is why the pump has one.
pub(super) fn should_say_it_cannot_connect(
    ever_connected: bool,
    said_so: bool,
    attempt: u32,
) -> bool {
    // A channel that has worked and then dropped is a laptop that slept, and
    // there is nothing for anyone to do about it.
    !ever_connected
        // Once per supervisor. At the backoff ceiling, "every time" is a line
        // every thirty seconds printed over whatever the developer is doing.
        && !said_so
        // Late enough that an ordinary slow reconnect stays quiet.
        && attempt >= QUIET_FAILURES
}

/// The sentence for a connection riabuild lost track of.
///
/// Deliberately says nothing about the server. That is the whole distinction
/// this variant exists to draw: `wait` failing is riabuild's own bookkeeping,
/// and a message naming the server would send a developer to the one machine
/// there is no evidence against. Like the unrecognised wall it is said once and
/// then the loop carries on, because a laptop that lost a child handle is a
/// laptop that will very likely start the next one fine.
pub(super) fn lost_track(detail: &str) -> Failure {
    Failure::new(
        "riabuild lost track of the clipboard channel's connection",
        "Nothing to do — riabuild rebuilds it on its own. If paste is still dead in a minute, \
         open a new riabuild shell. Everything except paste works without it.",
    )
    .detail(detail.trim().to_string())
}

/// The sentence for a connection that keeps failing in a way `diagnose` has no
/// pattern for.
///
/// Two of them, because one of the two is not a network fault at all and saying
/// it was sent developers looking at their wifi. A pump that outlived its
/// laptop — the connection dropped, the server never noticed, and the process
/// stayed bound to the socket — refuses every replacement with `already
/// serving`, so the `ssh` reaches the server perfectly and comes back with a
/// message about a *colleague's* session. "Cannot reach this server" is the one
/// thing that is definitely not happening.
///
/// It resolves itself now, which is why the wording says to wait rather than to
/// do something: the pump gives the socket up once its own keepalive goes
/// unanswered, and the next attempt binds it.
pub(super) fn cannot_connect(stderr: &str) -> Failure {
    if stderr.to_ascii_lowercase().contains("already serving") {
        return Failure::new(
            "another session on this server is still holding the channel",
            "Nothing to do — it is usually a session whose connection dropped without the \
             server noticing, and it gives the channel up within a minute. If paste is still \
             dead after that, run `riabuild channel status` on the server.",
        )
        .detail(stderr.trim().to_string());
    }

    Failure::new(
        "the clipboard channel cannot reach this server",
        "Run `riabuild channel status` on the server to check, and `riabuild remote` again \
         from here to rebuild it. Everything except paste works without it.",
    )
    .detail(stderr.trim().to_string())
}

/// Shows a failure without claiming riabuild stopped.
///
/// `Ui::failure` prints "riabuild stopped:", which would be a lie here. The
/// setup run, the secrets and the mosh session are all untouched — only paste
/// stops — and sending a developer to look for a broken environment they do not
/// have is worse than saying nothing.
///
/// **On the bar where there is one, and printed only where there is not.** This
/// runs beside the developer's remote shell, which means printing it lands
/// multi-line prose in the middle of a screen mosh and Claude Code are drawing,
/// through a terminal an interactive shell has put in raw mode — where `\n`
/// drops a row without returning to column one, so the folded sentence arrives
/// as a staircase and stays there. One line at a fixed row, with the cursor put
/// back, is the whole of the repair; the detail and the remedy are what the bar
/// cannot carry, and `riabuild channel status` is where a developer gets them.
pub(super) fn report(ui: &Ui, bar: &StatusBar, failure: Failure) -> Failure {
    if bar.enabled() {
        bar.show(&format!(
            "▲ Clipboard channel — {} · paste is off",
            failure.attempting
        ));
        return failure;
    }

    ui.warn(&format!("Clipboard channel — {failure}"));
    for line in failure
        .detail
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(4)
    {
        ui.note(line);
    }
    ui.info("Paste will not work for the rest of this session. Nothing else is affected.");
    failure
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failure nobody has written a sentence for still has to produce one.
    ///
    /// This is the gap that hid a real bug for the whole life of the exec
    /// transport: the channel's `ssh` was refusing an unverifiable host key,
    /// `diagnose` matched none of its patterns, and the loop retried in silence
    /// for the length of every session. Three rounds of "paste does not work"
    /// went by with nothing anywhere naming a cause.
    #[test]
    fn a_failure_nobody_recognises_is_still_said_once() {
        // Silent while an ordinary reconnect might still succeed.
        assert!(!should_say_it_cannot_connect(false, false, 0));
        assert!(!should_say_it_cannot_connect(
            false,
            false,
            QUIET_FAILURES - 1
        ));
        // Then said.
        assert!(should_say_it_cannot_connect(false, false, QUIET_FAILURES));
        // Once. At the backoff ceiling, "every time" is a line every thirty
        // seconds printed over whatever the developer is doing.
        assert!(!should_say_it_cannot_connect(false, true, QUIET_FAILURES));
        // And never for a channel that has worked: that is a laptop that
        // slept, and there is nothing for anyone to do about it.
        assert!(!should_say_it_cannot_connect(true, false, QUIET_FAILURES));
        assert!(!should_say_it_cannot_connect(true, false, 99));
    }

    /// The wall that is not a network fault, told apart from the one that is.
    ///
    /// `already serving` comes back from a server the `ssh` reached perfectly:
    /// a pump that outlived its laptop is still bound to the socket and refuses
    /// the replacement. Reported as "cannot reach this server" — which is what
    /// every unrecognised failure used to become — it sends a developer to look
    /// at their network, which is the one thing that is definitely working.
    #[test]
    fn a_socket_another_pump_still_holds_is_not_reported_as_an_unreachable_server() {
        let held = cannot_connect(
            "riabuild stopped: another riabuild is already serving the clipboard channel at /x",
        );
        assert!(
            !held.to_string().contains("cannot reach"),
            "{held} blames the network for a server that answered"
        );
        assert!(held.attempting.contains("another session"), "{held}");
        // And it says to wait rather than to do something, because the other
        // pump's own keepalive is what ends this.
        assert!(held.action.contains("within a minute"), "{held}");

        let unreachable = cannot_connect("ssh: connect to host build-01 port 22: No route to host");
        assert!(
            unreachable.attempting.contains("cannot reach"),
            "{unreachable}"
        );
    }

    /// …and the sentence it says once must not blame the server, which is the
    /// machine there is no evidence against.
    #[test]
    fn losing_track_of_a_child_says_nothing_about_the_server() {
        let failure = lost_track("No such file or directory (os error 2)");
        let said = failure.to_string();
        assert!(!said.contains("pump"), "{said}");
        assert!(!said.contains("cannot reach"), "{said}");
        assert!(failure.attempting.contains("lost track"), "{said}");
    }
}
