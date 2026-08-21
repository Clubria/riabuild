//! Where riabuild keeps its things.
//!
//! Behind a trait from the first commit so Linux support is an addition rather
//! than a rewrite, and so tests can point the whole tree at a tempdir.
//!
//! Four files answer "where", one concern each. `layout` is the [`Paths`]
//! trait — every path riabuild knows, derived from a root. `root` is where that
//! root comes from: this machine's own, and one developer's namespace on a
//! shared server. `project` is where a *checkout* goes, which is the one
//! decision this crate takes from the operating system. `text` is paths as
//! strings — the `~` a developer types, and the `PATH` a shell splits on `:`.
//!
//! `config` and `filelock` are the other half: the state riabuild keeps under
//! those paths, and the lock it is read and written under.

// `unwrap_used` is denied workspace-wide. In test scaffolding a panic *is* the
// reporting mechanism for a failed precondition, so unwrapping a fixture is
// correct — but the exemption is `test` and nothing else, and must stay that
// way.
//
// It read `any(test, feature = "testing")`, which switched the lint off for
// this crate's *production* code under the one command that enforces it.
// `cargo clippy --workspace --all-targets` resolves dev-dependencies, a
// dev-dependency somewhere in the workspace asks for `testing`, and features
// unify onto the lib target — so the whole crate compiled with the allow on.
// With `test` alone the lib target is linted again, and the unit-test target
// that keeps the allow holds no production code the lib target does not.
//
// Scaffolding behind `feature = "testing"` carries its own allow where it is
// defined, which is a hole the size of a module rather than of a crate.
#![cfg_attr(test, allow(clippy::expect_used, clippy::panic, clippy::unwrap_used))]

pub mod config;
pub mod filelock;

mod layout;
mod project;
mod root;
mod text;

pub use layout::Paths;
pub use project::{default_project_dir, remote_project_dir};
pub use root::{RealPaths, remote_namespace, root_for};
pub use text::{contract_tilde, expand_tilde, path_without};
