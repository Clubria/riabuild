//! Getting bytes from upstream, proving they are the right bytes, and
//! unpacking them.
//!
//! Three peers rather than one module with a primary: `download` decides where
//! bytes come from and whether they match a published digest, `archive` only
//! ever sees a buffer that already did, and `tools` names the releases riabuild
//! owns. Keeping that split is what makes "verified before anything is written"
//! a property of the code rather than a convention.
//!
//! This crate has no dependency on anything else in the workspace, and that is
//! deliberate: it cannot reach the API client, so a string the server sent can
//! never become a URL riabuild downloads from.

// `unwrap_used` is denied workspace-wide. In tests a panic *is* the reporting
// mechanism for a failed precondition, so unwrapping a fixture there is
// correct and this keeps the deny from forcing ceremony into every test module.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod archive;
pub mod download;
pub mod tools;
