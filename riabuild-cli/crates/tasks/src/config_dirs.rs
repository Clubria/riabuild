//! `riabuild paths` — every directory riabuild keeps configuration in, and the
//! variable that points a tool at each one.
//!
//! The question this answers is one a developer can otherwise only answer by
//! reading riabuild's source: *which directory is my second Claude Code login
//! in?* Each account is a uuid under `~/.riabuild/claude/`, chosen by riabuild
//! and named after nothing a person picked, and the launcher that points Claude
//! Code at it — `CLAUDE_CONFIG_DIR=<dir> claude` — is a generated script most
//! developers never open. Codex and Grok Build are the same shape under
//! `CODEX_HOME` and `GROK_HOME`.
//!
//! Nothing here is a *route* into those directories that did not already exist:
//! the launchers riabuild writes are still the way to run each tool, and this
//! command exists for the case they do not cover — pointing something riabuild
//! did not write at the login riabuild made, or telling a lead which directory
//! to look in when a developer's history is not where they expect.
//!
//! **These are the paths riabuild points each tool at, not a claim that any of
//! them exists.** A path is printed on a machine nothing has provisioned yet,
//! exactly as it is on one that is fully set up — the alternative is a listing
//! that stats a directory per line and answers a question `riabuild --check`
//! already answers better.

use crate::Ctx;
use crate::accounts::render::launcher_label;
use crate::accounts::status::{Account, Identity};
use crate::shims;
use anyhow::Result;
use riabuild_theme::{Role, Theme};
use std::path::PathBuf;

/// One directory, as one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What a developer calls this directory: `claude-2`, `codex-1`, `bin`.
    pub label: String,
    pub path: PathBuf,
    /// Anything worth saying about the line — for a Claude Code account, who is
    /// signed in there.
    pub note: Option<String>,
}

/// One tool's directories, and the variable that points it at one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub tool: String,
    /// `None` for riabuild's own tree, which no variable addresses.
    pub var: Option<&'static str>,
    pub entries: Vec<Entry>,
}

impl Group {
    fn heading(&self, theme: Theme) -> String {
        let tool = theme.paint(Role::Strong, &self.tool);
        match self.var {
            Some(var) => format!("{tool} {}", theme.paint(Role::Muted, &format!("— {var}"))),
            None => tool,
        }
    }
}

/// `riabuild paths`.
///
/// Behind no `connect`, for the reason `riabuild claude` is: every path here is
/// computed from this machine's own config, so the command answers with no
/// riabuild session, no network, and a machine nothing has provisioned.
pub async fn list(ctx: &Ctx) -> Result<i32> {
    // The one thing here that is asked rather than computed. It costs about
    // 450 ms for all accounts at once — see `accounts::status` — and it is what
    // turns a column of uuids into an answer: a developer looking for a
    // directory is looking for the *login* in it.
    let accounts = crate::accounts::status::read_all(ctx).await;
    ctx.ui.info("");
    ctx.ui
        .info(&render(&groups(ctx, &accounts), ctx.ui.theme()));
    ctx.ui.info("");
    ctx.ui.note(
        "Each launcher already sets these itself — `claude-2` runs Claude Code with \
         CLAUDE_CONFIG_DIR pointed at its account. Export one yourself only to point \
         something riabuild did not write at the same directory.",
    );
    Ok(0)
}

/// Every group, in the order a developer reads them: the three harnesses first,
/// because they are what the question is usually about, and riabuild's own tree
/// last.
pub fn groups(ctx: &Ctx, accounts: &[Account]) -> Vec<Group> {
    vec![
        claude(ctx, accounts),
        Group {
            tool: "Codex".to_string(),
            var: Some("CODEX_HOME"),
            entries: (1..=shims::codex::PROFILES)
                .map(|profile| Entry {
                    label: launcher_label("codex", profile),
                    path: ctx.paths.codex_profile_dir(profile),
                    note: None,
                })
                .collect(),
        },
        Group {
            tool: "Grok Build".to_string(),
            var: Some("GROK_HOME"),
            entries: (1..=shims::grok::PROFILES)
                .map(|profile| Entry {
                    label: launcher_label("grok", profile),
                    path: ctx.paths.grok_profile_dir(profile),
                    note: None,
                })
                .collect(),
        },
        riabuild(ctx),
    ]
}

fn claude(ctx: &Ctx, accounts: &[Account]) -> Group {
    Group {
        tool: "Claude Code".to_string(),
        var: Some("CLAUDE_CONFIG_DIR"),
        entries: accounts
            .iter()
            .map(|account| Entry {
                label: launcher_label("claude", account.number),
                path: ctx.paths.claude_profile_dir(&account.id),
                note: signed_in_as(&account.identity),
            })
            .collect(),
    }
}

/// riabuild's own tree.
///
/// The files are here beside the directories because they are the same
/// question asked one level down — "where does riabuild keep what it knows
/// about this machine" — and a developer who wants `config.json` wants its
/// path, not the directory it is in and a name to guess.
///
/// No secret is named. This machine's session token and a saved SSH password
/// are the two riabuild keeps, both usually in a keychain rather than a file,
/// and `riabuild status` is where the developer is told which — see "No
/// secrets in `~/.riabuild/`" in `riabuild-cli/CLAUDE.md`.
fn riabuild(ctx: &Ctx) -> Group {
    let mut entries = vec![Entry {
        label: "root".to_string(),
        path: ctx.paths.root(),
        note: None,
    }];
    // Named only where it is somewhere else, which is a managed server: there
    // `root()` is the developer's own namespace under `.riabuild-remote/<id>`
    // and the toolchain stays in the account's `~/.riabuild`, shared with
    // everyone else with a login on the box. On a laptop the two are one
    // directory and a second line saying so would be noise.
    if ctx.paths.tools_root() != ctx.paths.root() {
        entries.push(Entry {
            label: "shared tools".to_string(),
            path: ctx.paths.tools_root(),
            note: Some("shared by everyone on this machine".to_string()),
        });
    }
    entries.extend([
        Entry {
            label: "bin".to_string(),
            path: ctx.paths.bin_dir(),
            note: None,
        },
        Entry {
            label: "agents".to_string(),
            path: ctx.paths.agents_dir(),
            note: None,
        },
        Entry {
            label: "config".to_string(),
            path: ctx.paths.config_file(),
            note: None,
        },
        Entry {
            label: "state".to_string(),
            path: ctx.paths.state_file(),
            note: None,
        },
        Entry {
            label: "org settings".to_string(),
            path: ctx.paths.org_settings_file(),
            note: None,
        },
    ]);
    if let Some(checkout) = ctx.project_dir() {
        entries.push(Entry {
            label: "checkout".to_string(),
            path: checkout,
            note: None,
        });
    }
    Group {
        tool: "riabuild".to_string(),
        var: None,
        entries,
    }
}

/// What this view says about who is signed in.
///
/// `Identity::Unknown` says nothing at all, which is the one place this differs
/// from `accounts::render` — and deliberately. There, the subject *is* the
/// sign-in, so "(cannot tell — …)" is the honest answer and printing nothing
/// would look like a claim. Here the subject is a directory, whose path is
/// known whether or not Claude Code could be asked who is in it; a machine with
/// no Claude Code installed would otherwise carry the same sentence about a
/// different question on every account line.
fn signed_in_as(identity: &Identity) -> Option<String> {
    match identity {
        Identity::LoggedIn(email) => Some(email.clone()),
        Identity::LoggedOut => Some("(logged out)".to_string()),
        Identity::Unknown(_) => None,
    }
}

pub fn render(groups: &[Group], theme: Theme) -> String {
    // One label column for the whole page rather than one per group: every
    // label is a short name, and columns that move between sections read as
    // several tables rather than as one answer.
    let label_width = groups
        .iter()
        .flat_map(|group| group.entries.iter())
        .map(|entry| entry.label.chars().count())
        .max()
        .unwrap_or(0);

    let mut lines: Vec<String> = Vec::new();
    for group in groups {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(group.heading(theme));

        // Only the lines that carry a note need their path padded, so a group
        // with none — Codex, Grok Build — has no trailing whitespace at all.
        let path_width = group
            .entries
            .iter()
            .filter(|entry| entry.note.is_some())
            .map(|entry| entry.path.display().to_string().chars().count())
            .max()
            .unwrap_or(0);

        if group.entries.is_empty() {
            lines.push(format!(
                "  {}",
                theme.paint(
                    Role::Muted,
                    "(none yet — run `riabuild` to set this machine up)"
                )
            ));
            continue;
        }

        for entry in &group.entries {
            let label = pad(&entry.label, label_width);
            let path = entry.path.display().to_string();
            match &entry.note {
                Some(note) => lines.push(format!(
                    "  {label}  {}   {}",
                    pad(&path, path_width),
                    theme.paint(Role::Muted, note)
                )),
                None => lines.push(format!("  {label}  {path}")),
            }
        }
    }
    lines.join("\n")
}

/// Padded on the character count, never on `len()`: a checkout path can hold
/// any character a filesystem does, and padding a multi-byte one by its byte
/// length pushes the column that follows it left.
fn pad(text: &str, width: usize) -> String {
    let padding = " ".repeat(width.saturating_sub(text.chars().count()));
    format!("{text}{padding}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts;
    use crate::testing::{ctx_on_a_server, test_ctx};
    use riabuild_runner::FakeRunner;

    fn account(number: usize, id: &str, identity: Identity) -> Account {
        Account {
            number,
            id: id.to_string(),
            identity,
            tracked: false,
        }
    }

    fn group<'a>(groups: &'a [Group], tool: &str) -> &'a Group {
        groups
            .iter()
            .find(|group| group.tool == tool)
            .unwrap_or_else(|| panic!("a {tool} group"))
    }

    #[tokio::test]
    async fn every_claude_account_is_listed_with_the_directory_that_holds_its_login() {
        let (mut ctx, _home) = test_ctx().await;
        let one = accounts::new_id();
        let two = accounts::new_id();
        ctx.config.claude_accounts = vec![one.clone(), two.clone()];

        let found = groups(
            &ctx,
            &[
                account(1, &one, Identity::LoggedIn("clubria@proton.me".into())),
                account(2, &two, Identity::LoggedOut),
            ],
        );
        let claude = group(&found, "Claude Code");

        assert_eq!(claude.var, Some("CLAUDE_CONFIG_DIR"));
        assert_eq!(claude.entries.len(), 2);
        // The primary answers to two names, exactly as the accounts box says.
        assert_eq!(claude.entries[0].label, "claude-1 / claude");
        assert_eq!(claude.entries[0].path, ctx.paths.claude_profile_dir(&one));
        assert_eq!(
            claude.entries[0].note.as_deref(),
            Some("clubria@proton.me"),
            "the login is what makes a uuid an answer"
        );
        assert_eq!(claude.entries[1].label, "claude-2");
        assert_eq!(claude.entries[1].path, ctx.paths.claude_profile_dir(&two));
    }

    #[tokio::test]
    async fn all_nine_codex_and_grok_profiles_are_listed() {
        // Fixed sets riabuild creates once, so every one of them is a directory
        // a developer can point something at whether or not they have signed in
        // to it — there is nothing to enumerate off disk and nothing to leave
        // out.
        let (ctx, _home) = test_ctx().await;
        let found = groups(&ctx, &[]);

        let codex = group(&found, "Codex");
        assert_eq!(codex.var, Some("CODEX_HOME"));
        assert_eq!(codex.entries.len(), shims::codex::PROFILES);
        assert_eq!(codex.entries[0].label, "codex-1 / codex");
        assert_eq!(codex.entries[0].path, ctx.paths.codex_profile_dir(1));
        assert_eq!(codex.entries[8].label, "codex-9");

        let grok = group(&found, "Grok Build");
        assert_eq!(grok.var, Some("GROK_HOME"));
        assert_eq!(grok.entries.len(), shims::grok::PROFILES);
        assert_eq!(grok.entries[0].path, ctx.paths.grok_profile_dir(1));
    }

    #[tokio::test]
    async fn riabuilds_own_tree_names_the_checkout_it_set_up() {
        let (mut ctx, home) = test_ctx().await;
        let checkout = home.path().join("Clubria/ai-builders-hub");
        ctx.config.set_checkout(
            "Clubria/ai-builders-hub",
            checkout.to_string_lossy().as_ref(),
        );

        let found = groups(&ctx, &[]);
        let riabuild = group(&found, "riabuild");

        assert_eq!(riabuild.var, None);
        let labelled = |label: &str| {
            riabuild
                .entries
                .iter()
                .find(|entry| entry.label == label)
                .map(|entry| entry.path.clone())
        };
        assert_eq!(labelled("root"), Some(ctx.paths.root()));
        assert_eq!(labelled("config"), Some(ctx.paths.config_file()));
        assert_eq!(labelled("checkout"), Some(checkout));
    }

    #[tokio::test]
    async fn the_shared_toolchain_is_named_only_where_it_is_somewhere_else() {
        // A laptop: one directory, one line. Two lines here would be riabuild
        // inventing a distinction the machine does not have.
        let (laptop, _home) = test_ctx().await;
        assert!(
            !groups(&laptop, &[])
                .iter()
                .flat_map(|group| group.entries.iter())
                .any(|entry| entry.label == "shared tools")
        );

        // A managed server: `root()` is this developer's namespace and the
        // toolchain is the account's, shared with everyone who has a login on
        // the box. Which is exactly when a developer needs telling that the two
        // are not the same directory — and the laptop shape, where they are one
        // directory, is what hides that from every other test in this crate.
        let (server, home) = ctx_on_a_server(FakeRunner::new()).await;

        let found = groups(&server, &[]);
        let riabuild = group(&found, "riabuild");
        let shared = riabuild
            .entries
            .iter()
            .find(|entry| entry.label == "shared tools")
            .expect("a shared toolchain line on a server");
        assert_eq!(shared.path, home.path().join(".riabuild"));
        assert_ne!(shared.path, server.paths.root());
        // And every profile stays in the namespace, which is the other half of
        // what makes two developers on one box invisible to each other.
        assert!(
            group(&found, "Codex").entries[0]
                .path
                .starts_with(server.paths.root())
        );
    }

    #[tokio::test]
    async fn a_machine_with_no_accounts_yet_says_so_rather_than_printing_a_gap() {
        let (ctx, _home) = test_ctx().await;
        let text = render(&groups(&ctx, &[]), Theme::plain());
        assert!(text.contains("Claude Code — CLAUDE_CONFIG_DIR"), "{text}");
        assert!(
            text.contains("(none yet — run `riabuild` to set this machine up)"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_login_riabuild_could_not_ask_about_is_left_unannotated() {
        // "(cannot tell — …)" belongs to `riabuild claude`, where the sign-in
        // is the subject. Here the subject is the directory, and its path is
        // known either way — a machine with no Claude Code installed would
        // otherwise repeat that sentence on every account line.
        let (mut ctx, _home) = test_ctx().await;
        let id = accounts::new_id();
        ctx.config.claude_accounts = vec![id.clone()];

        let found = groups(
            &ctx,
            &[account(
                1,
                &id,
                Identity::Unknown("no Claude Code here".into()),
            )],
        );
        assert_eq!(group(&found, "Claude Code").entries[0].note, None);
    }

    #[test]
    fn the_paths_line_up_under_labels_of_different_widths() {
        let text = render(
            &[Group {
                tool: "Claude Code".to_string(),
                var: Some("CLAUDE_CONFIG_DIR"),
                entries: vec![
                    Entry {
                        label: "claude-1 / claude".to_string(),
                        path: PathBuf::from("/home/ada/.riabuild/claude/aaa"),
                        note: Some("clubria@proton.me".to_string()),
                    },
                    Entry {
                        label: "claude-2".to_string(),
                        path: PathBuf::from("/home/ada/.riabuild/claude/bbb"),
                        note: Some("(logged out)".to_string()),
                    },
                ],
            }],
            Theme::plain(),
        );

        assert!(
            text.contains(
                "  claude-1 / claude  /home/ada/.riabuild/claude/aaa   clubria@proton.me"
            ),
            "{text}"
        );
        assert!(
            text.contains("  claude-2           /home/ada/.riabuild/claude/bbb   (logged out)"),
            "{text}"
        );
    }
}
