//! The two platform credential tools: macOS `security(1)`, talking to the login
//! keychain, and Linux libsecret through `secret-tool`.
//!
//! Both drive the tool through `CommandRunner`, which keeps these files free of
//! platform crates and keeps the behaviour unit-testable. Neither is `cfg`-gated
//! for that reason: `for_platform` chooses between them at runtime, so the macOS
//! path still compiles and still has tests on the Linux host every pull request
//! is gated on.
//!
//! `secret-tool` is handed the token on **stdin**, never as an argument: argv
//! is world-readable through `ps`. `security` cannot be — it has no stdin path
//! for a password at all, only a `/dev/tty` prompt — so on macOS the token is
//! an argv element and the leak is accepted. `SecurityCliKeychain::set` carries
//! the whole argument.
//!
//! The three files under this directory are the three answers, one concern
//! each: `security` is the macOS store, `secret_tool` is the Linux one, and
//! `probe` is `keyring_answers` — the separate question of whether a Secret
//! Service is there to be talked to at all, which is not the same as either
//! store and is asked before one is chosen.

mod probe;
mod secret_tool;
mod security;

pub(crate) use probe::keyring_answers;
pub use secret_tool::SecretToolKeychain;
pub use security::SecurityCliKeychain;

const SERVICE: &str = "com.clubria.riabuild";
const ACCOUNT: &str = "session-token";
