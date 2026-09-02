//! The account list a developer sees at every shell start.
//!
//! The theme is a parameter rather than something read from `Ui`, for the same
//! reason `shell::banner` takes one: this text is printed by a generated rcfile,
//! and the colour decision has to cross that boundary as data.

use super::MAX;
use super::status::{Account, Identity};
use riabuild_theme::{Role, Theme};

pub fn accounts_box(accounts: &[Account], theme: Theme) -> String {
    let mut lines = vec![
        theme.paint(Role::Strong, "Your Claude Code accounts:"),
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
            "  {}. {label}{padding}   {}{}",
            account.number,
            identity(&account.identity, theme),
            tracked(account, theme)
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
                theme.paint(Role::Muted, &label)
            ));
        }
    }

    lines.join("\n")
}

/// The command that runs this account.
fn label(account: &Account) -> String {
    launcher_label("claude", account.number)
}

/// The command that runs one of a tool's numbered profiles.
///
/// The first answers to two names — `claude-1` and `claude`, `codex-1` and
/// `codex` — because every launcher riabuild writes gives it both, and a list
/// has to say which of them the bare name is.
///
/// Shared with `config_dirs`, which lists the same names against the
/// directories they point at: two spellings of "what is this profile called"
/// would eventually disagree, and the one a developer would meet is a paths
/// listing naming a launcher the accounts box does not.
pub fn launcher_label(tool: &str, number: usize) -> String {
    if number == 1 {
        format!("{tool}-1 / {tool}")
    } else {
        format!("{tool}-{number}")
    }
}

fn identity(identity: &Identity, theme: Theme) -> String {
    match identity {
        Identity::LoggedIn(email) => email.clone(),
        Identity::LoggedOut => theme.paint(Role::Muted, "(logged out)"),
        Identity::Unknown(why) => theme.paint(Role::Muted, &format!("(cannot tell — {why})")),
    }
}

/// Only the commands that would succeed right now.
///
/// A hint that refuses when typed is worse than no hint: it reads as riabuild
/// being broken rather than as the developer asking for something impossible.
/// The tag on an account whose usage reaches the dashboard.
///
/// Only on the accounts that carry it. An "(not tracked)" on every other line
/// would be four words of noise on the common case — nothing is tracked until
/// somebody asks — and would make the box harder to read to say less.
fn tracked(account: &Account, theme: Theme) -> String {
    match account.tracked {
        true => format!("  {}", theme.paint(Role::Muted, "· usage tracked")),
        false => String::new(),
    }
}

fn hints(accounts: &[Account]) -> Vec<(String, String)> {
    let mut hints = Vec::new();
    if accounts.len() < MAX {
        hints.push((
            "Add an account:".to_string(),
            "riabuild claude new".to_string(),
        ));
    }
    if accounts.len() > 1 {
        // Both numbers come out of the accounts themselves rather than from the
        // length or a literal. `&[Account]` does not promise a contiguous
        // `1..N`, and a caller that passed a filtered list would otherwise be
        // told to delete or promote an account the box above never showed —
        // exactly the hint-that-refuses this function exists to avoid.
        if let Some(last) = accounts.last() {
            hints.push((
                "Delete an account:".to_string(),
                format!("riabuild claude delete {}", last.number),
            ));
        }
        if let Some(second) = accounts.get(1) {
            hints.push((
                "Make it primary:".to_string(),
                format!("riabuild claude primary {}", second.number),
            ));
        }
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
    // Each shown only where it would do something, like every hint above it.
    // A developer with nothing tracked is never offered `untrack`, and one with
    // everything tracked is never offered `track`.
    if let Some(account) = accounts.iter().find(|account| !account.tracked) {
        hints.push((
            "Report its usage:".to_string(),
            format!("riabuild claude track {}", account.number),
        ));
    }
    if let Some(account) = accounts.iter().find(|account| account.tracked) {
        hints.push((
            "Stop reporting:".to_string(),
            format!("riabuild claude untrack {}", account.number),
        ));
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;
    use riabuild_theme::Depth;

    fn account(number: usize, identity: Identity) -> Account {
        Account {
            number,
            id: format!("id-{number}"),
            identity,
            tracked: false,
        }
    }

    fn tracked_account(number: usize, identity: Identity) -> Account {
        Account {
            tracked: true,
            ..account(number, identity)
        }
    }

    /// The default is invisible, and that is the point: nothing is tracked
    /// until somebody asks, so the common box says nothing about it.
    #[test]
    fn an_untracked_account_carries_no_tag() {
        let drawn = accounts_box(
            &[account(1, Identity::LoggedIn("ada@example.com".into()))],
            Theme::plain(),
        );

        assert!(!drawn.contains("usage tracked"), "{drawn}");
    }

    /// A developer can see, in the place they already look, which of their
    /// accounts reports usage to their employer.
    #[test]
    fn a_tracked_account_says_so_beside_its_email() {
        let drawn = accounts_box(
            &[
                tracked_account(1, Identity::LoggedIn("ada@clubria.com".into())),
                account(2, Identity::LoggedIn("ada@personal.example".into())),
            ],
            Theme::plain(),
        );

        let tracked_line = drawn
            .lines()
            .find(|line| line.contains("ada@clubria.com"))
            .expect("the tracked account is listed");
        assert!(tracked_line.contains("usage tracked"), "{drawn}");

        let personal_line = drawn
            .lines()
            .find(|line| line.contains("ada@personal.example"))
            .expect("the personal account is listed");
        assert!(
            !personal_line.contains("usage tracked"),
            "a personal account must not be tagged: {drawn}"
        );
    }

    /// Every hint in this box is a command that works right now. A developer
    /// with nothing tracked has nothing to untrack.
    #[test]
    fn untrack_is_not_offered_when_nothing_is_tracked() {
        let drawn = accounts_box(
            &[account(1, Identity::LoggedIn("ada@example.com".into()))],
            Theme::plain(),
        );

        assert!(drawn.contains("riabuild claude track 1"), "{drawn}");
        assert!(!drawn.contains("untrack"), "{drawn}");
    }

    /// And the reverse.
    #[test]
    fn track_is_not_offered_when_everything_is_tracked() {
        let drawn = accounts_box(
            &[tracked_account(
                1,
                Identity::LoggedIn("ada@example.com".into()),
            )],
            Theme::plain(),
        );

        assert!(drawn.contains("riabuild claude untrack 1"), "{drawn}");
        assert!(
            !drawn.contains("claude track "),
            "nothing left to track: {drawn}"
        );
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
        let text = accounts_box(&three(), Theme::plain());
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
        let text = accounts_box(&three(), Theme::plain());
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
    fn every_hint_names_an_account_that_is_in_the_box() {
        // The numbers a developer would type have to come from the accounts
        // shown, not from how many there are: a list whose numbers do not start
        // at 1 and run on would otherwise be told to delete an account it never
        // listed, which is the hint-that-refuses this function exists to avoid.
        let odd = vec![
            account(4, Identity::LoggedIn("a@example.com".into())),
            account(7, Identity::LoggedIn("b@example.com".into())),
        ];
        let text = accounts_box(&odd, Theme::plain());
        assert!(text.contains("riabuild claude delete 7"), "{text}");
        assert!(text.contains("riabuild claude primary 7"), "{text}");
    }

    #[test]
    fn a_single_account_is_offered_neither_delete_nor_primary() {
        // Both refuse or do nothing with one account, and a hint that fails is
        // worse than no hint.
        let one = vec![account(1, Identity::LoggedIn("clubria@proton.me".into()))];
        let text = accounts_box(&one, Theme::plain());
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
        assert!(!accounts_box(&signed_in, Theme::plain()).contains("auth login"));
    }

    #[test]
    fn a_full_list_is_not_offered_another_account() {
        let full: Vec<Account> = (1..=MAX)
            .map(|number| account(number, Identity::LoggedIn(format!("{number}@example.com"))))
            .collect();
        assert!(!accounts_box(&full, Theme::plain()).contains("riabuild claude new"));
    }

    #[test]
    fn not_knowing_is_said_out_loud() {
        let unsure = vec![account(
            1,
            Identity::Unknown("Claude Code did not answer in JSON".into()),
        )];
        let text = accounts_box(&unsure, Theme::plain());
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
        assert!(!accounts_box(&three(), Theme::plain()).contains('\x1b'));
        assert!(accounts_box(&three(), Theme::with_depth(Depth::TrueColor)).contains('\x1b'));
    }
}
