//! Which Claude Code account this window is signed in as: `claude-2 · ada@clubria.com`.
//!
//! The question the marker cannot answer. A developer runs `claude-1` in one
//! window and `claude-2` in another — two logins, two subscriptions, often two
//! organisations — and the launchers that tell them apart are generated scripts
//! nobody opens. Every window then looks identical, and the way that is
//! discovered is by having asked the wrong account to do something.
//!
//! **Read out of `CLAUDE_CONFIG_DIR`, never from `Paths`.** The status line
//! serves whichever developer's session started it, and on a server one Unix
//! account holds several of them. The launcher sets the variable on *this
//! session's* environment, so the namespace arrives from the running session
//! rather than from a guess about whose process this is. The same binary then
//! serves two colleagues on one box and answers differently for each, which is
//! the property this has to have.

use serde_json::Value;
use std::path::Path;

/// ` claude-2 · ada@clubria.com`, dim, and only the halves that are known.
///
/// The two halves fail independently on purpose. A logged-out account still
/// names its launcher, because `claude-2` with nothing after it is the answer to
/// "which window is this?" and is also how a developer notices they are signed
/// out. An account riabuild's config does not list still shows its email, which
/// is what a `claude` started outside the launchers looks like. Neither known:
/// nothing is drawn, and the line is the one that shipped before any of this.
pub(super) fn line(config_dir: Option<&Path>) -> String {
    let Some(dir) = config_dir else {
        return String::new();
    };

    let mut parts = Vec::new();
    if let Some(number) = number_of(dir) {
        parts.push(format!("claude-{number}"));
    }
    if let Some(email) = email_in(dir) {
        parts.push(email);
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(" \x1b[2m{}\x1b[0m", parts.join(" · "))
}

/// Which launcher opens this account — the `2` in `claude-2`.
///
/// Position in `claude_accounts` *is* the number, exactly as
/// `UserConfig::claude_accounts` records it: account 3 is index 2, and removing
/// one renumbers the rest by moving them. Nothing persists the number, so the
/// only way to name the launcher a developer would actually type is to find the
/// directory in that list.
///
/// `config.json` sits at `root()` and `CLAUDE_CONFIG_DIR` is
/// `root()/claude/<uuid>`, so two levels up is the namespace this session
/// belongs to — on a laptop and on a server alike. Derived rather than assumed,
/// because `~/.riabuild/config.json` is the right file on a laptop and the
/// wrong developer's on a server.
fn number_of(dir: &Path) -> Option<usize> {
    let uuid = dir.file_name()?.to_str()?;
    let config = dir.parent()?.parent()?.join("config.json");
    let text = std::fs::read_to_string(config).ok()?;
    let accounts = serde_json::from_str::<Value>(&text)
        .ok()?
        .get("claude_accounts")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .position(|held| held == uuid)?;
    Some(accounts + 1)
}

/// The email Claude Code recorded for the account signed in there.
///
/// Read as a file, for the reason `repo` reads `.git/config` as one:
/// `claude auth status --json` is the supported way to ask this and
/// `accounts::status` uses it, where it costs one Claude Code startup — about
/// 450 ms — once per run. A status line re-renders continuously, so the same
/// call here is that cost *per render*, and it would be Claude Code starting
/// itself to answer a question about itself.
///
/// `oauthAccount.emailAddress` is Claude Code's own state and nothing promises
/// to keep the key, which `accounts::status` says out loud and is why that is
/// not the route riabuild takes when it can afford the subprocess. What makes
/// the weaker source acceptable *here* is the failure it has: a key that moves
/// takes the email off the status line and leaves everything else drawn.
/// Nothing breaks and nothing is misreported — whereas a signed-out account and
/// a renamed key must never be told apart by guessing, so neither is: both draw
/// nothing.
fn email_in(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".claude.json")).ok()?;
    // Claude Code rewrites this file while it runs. A read that lands mid-write
    // is a parse error and not a signed-out account, so it draws nothing rather
    // than saying something wrong for the one render it affects.
    let email = serde_json::from_str::<Value>(&text)
        .ok()?
        .get("oauthAccount")?
        .get("emailAddress")?
        .as_str()?
        .to_string();
    (!email.is_empty()).then_some(email)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statusline::testing::{namespace, write};

    /// Two accounts, and the second one has to say so — the number comes from
    /// position in `claude_accounts` and nothing else records it.
    #[test]
    fn the_account_names_the_launcher_and_the_email() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(
            &home.path().join("ns"),
            &[
                ("one-uuid", Some("ada@clubria.com")),
                ("two-uuid", Some("ada@personal.example")),
            ],
        );

        let drawn = line(Some(&dirs[1]));

        assert!(drawn.contains("claude-2"), "{drawn:?}");
        assert!(drawn.contains("ada@personal.example"), "{drawn:?}");
    }

    /// Signing out is exactly when a developer needs to be told which window
    /// they are in, so the launcher name has to survive it.
    #[test]
    fn a_signed_out_account_still_names_its_launcher() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(&home.path().join("ns"), &[("one-uuid", None)]);

        let drawn = line(Some(&dirs[0]));

        assert!(drawn.contains("claude-1"), "{drawn:?}");
    }

    /// A `claude` riabuild's config does not list is a real thing to be — a
    /// developer's own install, an account added by hand — and it still knows
    /// who is signed in.
    #[test]
    fn an_account_riabuild_does_not_list_still_names_who_is_signed_in() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(&home.path().join("ns"), &[("one-uuid", None)]);
        let stranger = dirs[0].parent().unwrap().join("not-listed");
        write(
            &stranger.join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"someone@example.com"}}"#,
        );

        let drawn = line(Some(&stranger));

        assert!(drawn.contains("someone@example.com"), "{drawn:?}");
        assert!(!drawn.contains("claude-"), "{drawn:?}");
    }

    /// Two developers on one server: one shared status line, two namespaces,
    /// and the answer comes from the session rather than from the process.
    #[test]
    fn two_developers_on_one_server_get_their_own_account() {
        let home = tempfile::TempDir::new().unwrap();
        let ada = namespace(
            &home.path().join("ada"),
            &[("a-uuid", Some("ada@clubria.com"))],
        );
        let bo = namespace(
            &home.path().join("bo"),
            &[("b-uuid", Some("bo@clubria.com"))],
        );

        assert!(line(Some(&ada[0])).contains("ada@clubria.com"));
        assert!(line(Some(&bo[0])).contains("bo@clubria.com"));
    }

    /// A `claude` the launchers did not start has no `CLAUDE_CONFIG_DIR`, so it
    /// has no account number and gets none rather than a guess.
    #[test]
    fn a_claude_the_launchers_did_not_start_draws_no_account() {
        assert_eq!(line(None), "");
    }

    /// Claude Code rewrites `.claude.json` while it runs, so a render can land
    /// mid-write. Half an email is worse than none.
    #[test]
    fn a_half_written_config_draws_no_email_rather_than_half_of_one() {
        let home = tempfile::TempDir::new().unwrap();
        let dirs = namespace(&home.path().join("ns"), &[("one-uuid", None)]);
        write(
            &dirs[0].join(".claude.json"),
            r#"{"oauthAccount":{"emailAd"#,
        );

        let drawn = line(Some(&dirs[0]));

        assert!(drawn.contains("claude-1"), "{drawn:?}");
        assert!(!drawn.contains('@'), "{drawn:?}");
    }

    /// The two halves fail independently: a namespace with no `config.json` has
    /// no number, and the email is still known.
    #[test]
    fn a_missing_config_costs_the_number_and_not_the_email() {
        let home = tempfile::TempDir::new().unwrap();
        let dir = home.path().join("ns").join("claude").join("one-uuid");
        write(
            &dir.join(".claude.json"),
            r#"{"oauthAccount":{"emailAddress":"ada@clubria.com"}}"#,
        );

        let drawn = line(Some(&dir));

        assert!(drawn.contains("ada@clubria.com"), "{drawn:?}");
        assert!(!drawn.contains("claude-"), "{drawn:?}");
    }
}
