//! Getting bytes from upstream, proving they are the right bytes, and
//! unpacking them.
//!
//! Three peers rather than one module with a primary: `download` decides where
//! bytes come from and whether they match a published digest, `archive` only
//! ever sees a buffer that already did, and `tools` names the releases riabuild
//! owns. Keeping that split is what makes "verified before anything is written"
//! a property of the code rather than a convention.
//!
//! This crate names exactly one other crate in the workspace — `riabuild-ui`,
//! for [`riabuild_ui::Failure`] — and that is the whole list. What the rule was
//! written to protect is untouched: `riabuild-ui` depends on nothing but
//! `riabuild-theme` and `riabuild-version`, so this crate still cannot reach
//! the API client, and a string the server sent can never become a URL riabuild
//! downloads from.
//!
//! It carries a `Failure` because downloading and unpacking is the most
//! failure-prone part of provisioning and none of it could produce one. A
//! flaky connection, a corporate proxy, an upstream rename and a full disk all
//! left here as a bare `anyhow` chain, reached `main`'s unknown-error branch,
//! and were reported to the developer as *"Send this to your team lead — it is
//! a bug in riabuild."*

// The panic lints are denied workspace-wide. In tests a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture there
// is correct and this keeps the deny from forcing ceremony into every test
// module. The exemption is `test` and nothing wider — see the workspace
// manifest for what an `any(test, feature = "testing")` spelling of it costs.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

pub mod archive;
pub mod download;
pub mod tools;

pub(crate) use riabuild_ui::Failure;

/// The action on every failure that is a fact about this machine rather than
/// something the developer did: a CPU or an operating system riabuild publishes
/// nothing for, or an archive shape it has not learned.
pub(crate) const TELL_YOUR_LEAD: &str = "Send this to your team lead — riabuild has to be taught about this machine before it can \
     set it up.";

/// The action on every failure between riabuild and a host it does not control.
///
/// One sentence for all of them because the causes are the same short list, and
/// naming them is the whole value: a developer who reads "check your network"
/// on a laptop whose browser works fine has been told nothing.
pub(crate) const CHECK_THE_NETWORK: &str = "Check that this machine can reach the internet — a VPN that does not route to it, a \
     corporate proxy, or an offline network are the usual causes — then run `riabuild` again.";

/// The action for an upstream release that is no longer where riabuild pinned
/// it. Nothing the developer can do, and re-running will fail identically.
pub(crate) const UPSTREAM_MOVED: &str = "Send this to your team lead — the release riabuild is pinned to has moved or been withdrawn \
     upstream, and the pin has to be updated.";
