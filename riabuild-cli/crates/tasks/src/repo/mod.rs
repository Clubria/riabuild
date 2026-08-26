//! Which repository a run is about, and how a developer says so.
//!
//! Three parts, split the way `remote/` splits its picker: [`list`] asks GitHub
//! what this developer may see, [`render`] draws the box, and [`pick`] decides
//! what an answer meant and records it. The decision half is pure, so it is
//! tested without a terminal.
//!
//! Nothing here is a task. The question is put once, before the task engine
//! runs, because it decides what "the project" means for every task that reads a
//! checkout — and because a task that asked something on every run would be a
//! task that is never satisfied.
//!
//! Asking and recording are separate for a second caller's sake: `riabuild
//! remote` puts this question on the laptop *for a server*, where every write
//! `pick::choose` makes would be about the wrong machine. `pick::offer` is the
//! box and the question with nothing written down, and `remote::repo` is what
//! does the recording there instead.

pub mod list;
pub mod pick;
pub mod render;

pub use pick::choose;
