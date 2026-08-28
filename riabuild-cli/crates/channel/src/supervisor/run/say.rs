//! What the supervisor says about a channel that will not come up, and when.
//!
//! Two sentences and one predicate. The predicate is here rather than inline
//! in the loop because it is the whole of the decision and the loop around it
//! cannot be unit-tested without an `ssh`; the sentences are here because
//! choosing between them is the same kind of judgement — which of two walls
//! this is, and whether it is about the server at all.
//!
//! **Two, because the third was not a failure.** It said that another session
//! on this server was still holding the channel, which is the ordinary state of
//! a developer's second terminal into one box and is the case remote mode
//! exists to serve. `supervise` answers that one before anything is said, with
//! [`Outcome::AlreadyServed`](super::Outcome::AlreadyServed); nothing that
//! reaches this file is a working channel.
//!
//! [`report`] is where both land. It never claims riabuild stopped, and it
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
/// One sentence, where there were two. The other said that *another session on
/// this server was still holding the channel*, and it never belonged here,
/// because that is not a failure. `supervise` now answers
/// [`ALREADY_SERVED`](crate::pump::ALREADY_SERVED) before anything is said at
/// all: a socket a sibling terminal's pump is serving is a channel that works
/// in this terminal too, so the session hands its lease back and stands by
/// instead of reporting anything. Said here, it meant a developer with two
/// windows open — which is what remote mode is *for* — read "paste is off"
/// while pasting.
///
/// What that branch was really covering is a pump that outlived its laptop: the
/// connection dropped, the server never noticed, and the process stayed bound
/// to the socket. That is met the same way and still resolves itself, one floor
/// up — the pump gives the socket back when its own keepalive goes unanswered,
/// and the standing-by session's next ask binds it. `hold` is what says so on
/// the one path where it does not resolve.
pub(super) fn cannot_connect(stderr: &str) -> Failure {
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

    /// A server that genuinely cannot be reached still says so.
    ///
    /// The other half of what this function used to decide is gone: a socket
    /// another of this laptop's own pumps is serving never reaches here any
    /// more, because it is not a failure to report. See
    /// `a_sibling_serving_the_socket_is_never_reported_as_a_failure` next door,
    /// which pins that at the level that now decides it.
    #[test]
    fn a_server_that_cannot_be_reached_is_still_named_as_one() {
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
