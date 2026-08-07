//! The account list a developer sees at every shell start.
//!
//! `colour` is a parameter rather than something read from `Ui`, for the same
//! reason `shell::banner` takes one: this text is printed by a generated rcfile,
//! and the `NO_COLOR` decision has to cross that boundary as data.

use super::MAX;
use super::status::{Account, Identity};

pub fn accounts_box(accounts: &[Account], colour: bool) -> String {
    let mut lines = vec![
        paint("Your Claude Code accounts:", "1", colour),
        String::new(),
    ];

    let width = accounts
        .iter()
        .map(|account| label(account).chars().count())
        .max()
        .unwrap_or(0);
    for account in accounts {
        let label = label(account);
        let padding = " ".repeat(width - label.chars().count());
        lines.push(format!(
            "  {}. {label}{padding}   {}",
            account.number,
            identity(&account.identity, colour)
        ));
    }

    let hints = hints(accounts);
    if !hints.is_empty() {
        lines.push(String::new());
        let width = hints
            .iter()
            .map(|(label, _)| label.chars().count())
            .max()
            .unwrap_or(0);
        for (label, command) in hints {
            let padding = " ".repeat(width - label.chars().count());
            lines.push(format!(
                "  {}{padding}  {command}",
                paint(&label, "2", colour)
            ));
        }
    }

    lines.join("\n")
}

/// The command that runs this account. The primary answers to two names.
fn label(account: &Account) -> String {
    if account.number == 1 {
        "claude-1 / claude".to_string()
    } else {
        format!("claude-{}", account.number)
    }
}

fn identity(identity: &Identity, colour: bool) -> String {
    match identity {
        Identity::LoggedIn(email) => email.clone(),
        Identity::LoggedOut => paint("(logged out)", "2", colour),
        Identity::Unknown(why) => paint(&format!("(cannot tell — {why})"), "2", colour),
    }
}

/// Only the commands that would succeed right now.
///
/// A hint that refuses when typed is worse than no hint: it reads as riabuild
/// being broken rather than as the developer asking for something impossible.
fn hints(accounts: &[Account]) -> Vec<(String, String)> {
    let mut hints = Vec::new();
    if accounts.len() < MAX {
        hints.push((
            "Add an account:".to_string(),
            "riabuild claude new".to_string(),
        ));
    }
    if accounts.len() > 1 {
        hints.push((
            "Delete an account:".to_string(),
            format!("riabuild claude delete {}", accounts.len()),
        ));
        hints.push((
            "Make it primary:".to_string(),
            "riabuild claude primary 2".to_string(),
        ));
    }
    if let Some(account) = accounts
        .iter()
        .find(|account| account.identity == Identity::LoggedOut)
    {
        hints.push((
            "Log in:".to_string(),
            format!("claude-{} auth login", account.number),
        ));
    }
    hints
}

fn paint(text: &str, code: &str, colour: bool) -> String {
    if colour {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(number: usize, identity: Identity) -> Account {
        Account {
            number,
            id: format!("id-{number}"),
            identity,
        }
    }

    fn three() -> Vec<Account> {
        vec![
            account(1, Identity::LoggedIn("clubria@proton.me".into())),
            account(2, Identity::LoggedIn("other@gmail.com".into())),
            account(3, Identity::LoggedOut),
        ]
    }

    #[test]
    fn every_account_is_listed_with_the_command_that_runs_it() {
        let text = accounts_box(&three(), false);
        assert!(text.contains("Your Claude Code accounts:"), "{text}");
        // The primary carries both names, because both work.
        assert!(
            text.contains("1. claude-1 / claude   clubria@proton.me"),
            "{text}"
        );
        assert!(
            text.contains("2. claude-2            other@gmail.com"),
            "{text}"
        );
        assert!(
            text.contains("3. claude-3            (logged out)"),
            "{text}"
        );
    }

    #[test]
    fn only_commands_that_would_work_are_offered() {
        let text = accounts_box(&three(), false);
        assert!(
            text.contains("Add an account:     riabuild claude new"),
            "{text}"
        );
        assert!(
            text.contains("Delete an account:  riabuild claude delete 3"),
            "{text}"
        );
        assert!(
            text.contains("Make it primary:    riabuild claude primary 2"),
            "{text}"
        );
        assert!(
            text.contains("Log in:             claude-3 auth login"),
            "{text}"
        );
    }

    #[test]
    fn a_single_account_is_offered_neither_delete_nor_primary() {
        // Both refuse or do nothing with one account, and a hint that fails is
        // worse than no hint.
        let one = vec![account(1, Identity::LoggedIn("clubria@proton.me".into()))];
        let text = accounts_box(&one, false);
        assert!(text.contains("riabuild claude new"), "{text}");
        assert!(!text.contains("delete"), "{text}");
        assert!(!text.contains("primary"), "{text}");
    }

    #[test]
    fn a_fully_signed_in_list_is_not_told_how_to_log_in() {
        let signed_in = vec![
            account(1, Identity::LoggedIn("a@example.com".into())),
            account(2, Identity::LoggedIn("b@example.com".into())),
        ];
        assert!(!accounts_box(&signed_in, false).contains("auth login"));
    }

    #[test]
    fn a_full_list_is_not_offered_another_account() {
        let full: Vec<Account> = (1..=MAX)
            .map(|number| account(number, Identity::LoggedIn(format!("{number}@example.com"))))
            .collect();
        assert!(!accounts_box(&full, false).contains("riabuild claude new"));
    }

    #[test]
    fn not_knowing_is_said_out_loud() {
        let unsure = vec![account(
            1,
            Identity::Unknown("Claude Code did not answer in JSON".into()),
        )];
        let text = accounts_box(&unsure, false);
        assert!(
            text.contains("(cannot tell — Claude Code did not answer in JSON)"),
            "{text}"
        );
        assert!(!text.contains("logged out"), "{text}");
    }

    #[test]
    fn without_colour_there_are_no_escapes() {
        // This text is baked into a generated rcfile, so NO_COLOR has to be
        // decided here rather than by whatever ends up printing it.
        assert!(!accounts_box(&three(), false).contains('\x1b'));
        assert!(accounts_box(&three(), true).contains('\x1b'));
    }
}
