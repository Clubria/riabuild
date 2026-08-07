//! The laptop channel: a request path from a remote server back to the
//! developer's laptop.
//!
//! The server asks and the laptop decides. The operation set is compiled into
//! the binary, so a server can request only what the laptop already implements
//! — it cannot push work, extend the operation set, or execute anything. That
//! asymmetry is what makes a reverse tunnel defensible at all, and it is the
//! architecture rule "the server ships data, never logic" applied to the one
//! direction remote mode had not opened.
//!
//! The channel is strictly optional. Its absence degrades to "no clipboard"
//! and never to "environment broken": a laptop that closes its lid leaves a
//! session that still runs setup, still re-pulls rotated secrets, and still
//! opens a shell. Only paste stops.

pub mod mime;
pub mod protocol;
