//! The three things riabuild says about the outcome, and why they are three.
//!
//! `paste` is the remedy as a stop, `carry_on` is the same remedy as a warning
//! beside a step that is over, and `fallback` is the one sentence both of them
//! have to get right: telling a developer riabuild will "ask for the password
//! once and remember it" on a server that has no password to ask for is the
//! sentence the issued-keys feature exists to stop printing.

use riabuild_ui::{Detail, Failure, Ui};

use super::Remote;

/// "Add this line by hand", as a stop.
///
/// A free function rather than the closure it used to be, because [`finish`]
/// needs the identical wording and a closure capturing `authorise`'s locals
/// could not be reached from there. Two copies of a remedy is how the two drift
/// apart.
pub(super) fn paste(remote: &Remote, public_key: &str) -> Failure {
    Failure::new(
        format!("authorising riabuild's key on {}", remote.host),
        format!(
            "Add this line to ~/.ssh/authorized_keys on {}, then run `riabuild remote` again:\n{}",
            remote.host,
            public_key.trim()
        ),
    )
}

/// What the rest of the run will use, now that riabuild's own key will not.
///
/// The two warnings below both have to name it, and they must not name the
/// wrong one: telling a developer riabuild will "ask for the password once and
/// remember it" on a server that has no password to ask for is the sentence
/// this whole feature exists to stop printing.
///
/// A whole sentence, capital and full stop, because both warnings set it as a
/// paragraph of its own rather than splicing it into one.
pub(super) fn fallback(remote: &Remote, entry: Option<&crate::issued::Working>) -> String {
    match entry {
        Some(entry) => format!(
            "The rest of this run will use the {} key issued to you.",
            entry.label
        ),
        None => format!(
            "The rest of this run will use {}'s password instead; riabuild asks for it \
             once and remembers it.",
            remote.target()
        ),
    }
}

/// The same remedy as [`paste`], said as a warning rather than as a stop, and
/// said in the place the `● Authorised` would have gone — the step is over
/// either way, and a task left at `◐` is the one outcome that is not true.
///
/// Deliberately built from the same `public_key`, and deliberately still naming
/// `authorized_keys`: the developer is getting in either way, and this is how
/// they stop needing to.
///
/// `outcome` is what the task line says and `because` is what the explanation
/// opens with. They are not the same sentence twice: one has to fit beside the
/// mark, and the other carries the error the copy actually returned.
pub(super) fn carry_on(
    ui: &Ui,
    remote: &Remote,
    public_key: &str,
    outcome: &str,
    because: &str,
    fallback: &str,
) {
    ui.unresolved(
        "Authorised",
        outcome,
        &[
            Detail::Prose(&format!(
                "riabuild's key cannot sign in to {} yet — {because}.",
                remote.host
            )),
            Detail::Prose(fallback),
            Detail::Prose(&format!(
                "To stop relying on that, add this line to ~/.ssh/authorized_keys on {}:",
                remote.host
            )),
            Detail::Verbatim(public_key.trim()),
        ],
    );
}
