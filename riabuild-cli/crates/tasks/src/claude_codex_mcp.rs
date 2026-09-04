//! Task 19 — Codex, offered to Claude Code as a subagent.
//!
//! Claude Code reaches another agent through an MCP server, and the one it is
//! given here is riabuild's own: `riabuild internal mcp-codex`, which opens a
//! Codex session in the `riabuild agents` store, runs a turn in it, and returns
//! the last thing Codex said. The transcript stays in the window rather than
//! going into the calling agent's context. `riabuild-mcp` is where that is
//! written out in full.
//!
//! **This is the one key riabuild-web may never supply.** `mcpServers` is on
//! `org_settings::vetting::EXECUTES_A_PROGRAM` — refused loudly rather than
//! stripped — because an entry there is a command and an argv Claude Code
//! spawns at session start. The whole point of that list is that the server
//! cannot choose what executes on a laptop, so the *only* two legal sources for
//! an MCP server are the checkout (which arrives through a pull request, and is
//! what `claude_plugins` reads) and this binary. The entry written here names
//! riabuild itself, by the absolute path riabuild is running from, and takes no
//! argument any server chose.
//!
//! **Local scope, which is per project.** Claude Code keeps three kinds of MCP
//! server, and this is the one that lives at
//! `projects.<checkout>.mcpServers` in the account's `.claude.json` — the same
//! object `claude_trust` already creates empty in `new_project_entry`, and the
//! same one `claude mcp add -s local` writes. A user-scope server would follow
//! the developer into every repository on the machine, including ones that are
//! not Clubria's; a `.mcp.json` in the checkout would need approval before it
//! loaded, and the approval is the dialog riabuild exists to have already
//! answered.
//!
//! **It repairs its own path.** The command is `/…/riabuild/<version>/riabuild`,
//! which moves with every upgrade — so `check()` compares the recorded command
//! and argv against what this riabuild would write, and an entry left behind by
//! last month's release is drift this repairs rather than a state it tolerates.
//! That is the same reasoning the generated launchers are written under.
//!
//! **It never overwrites an entry it did not write.** A `codex` server whose
//! command is not this shape is a developer's own, and riabuild stands aside
//! from it entirely — the same standing-aside `claude_agents_view` does for a
//! `/config` answer. What that does not provide is an off switch: deleting
//! riabuild's entry brings it back on the next run, and a developer who does not
//! want a Codex subagent has the ordinary remedy, which is not to call the tool.

use super::claude_config::{self, Stored};
use super::{Ctx, Resource, Status, Task, TaskId};
use anyhow::Result;
use async_trait::async_trait;
use riabuild_ui::Failure;
use serde_json::{Map, Value, json};

pub struct ClaudeCodexMcp;

/// What the server is called, in the account's config and in the tool names
/// Claude Code derives from it — `mcp__codex__codex`.
const NAME: &str = "codex";

/// The argv riabuild's own entry carries, after the binary.
///
/// `--profile 1` is written out rather than left to the flag's default so that
/// the entry says which sign-in it opens, in a file a developer may read. The
/// first profile is `codex`, which is the one a machine that has never made a
/// second has.
fn arguments() -> Vec<String> {
    ["internal", "mcp-codex", "--profile", "1"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// The entry this riabuild would write.
fn entry(riabuild: &std::path::Path) -> Value {
    json!({
        // Named rather than left out. Claude Code defaults an entry with no
        // `type` to stdio, and a default is a thing that can change in a
        // release; this file is read by a binary riabuild does not ship.
        "type": "stdio",
        "command": riabuild.display().to_string(),
        "args": arguments(),
        // Empty and present, which is the shape `claude mcp add` writes. The
        // environment this server actually needs is the one it inherits:
        // `RIABUILD_AGENT_SESSION`, set by the turn that started Claude Code,
        // is what tells it which session is delegating — and no task could
        // write that here, because it differs per session.
        "env": {}
    })
}

/// Whether an entry already in the file is one riabuild wrote and still agrees
/// with.
///
/// Both halves matter. The command identifies it as riabuild's — an entry
/// pointing anywhere else belongs to the developer and is left alone — and the
/// argv is what makes an upgrade, or a change to the flags this task writes,
/// show up as drift instead of as a machine that silently keeps running the old
/// shape.
fn is_current(found: &Value, riabuild: &std::path::Path) -> bool {
    let command = found.get("command").and_then(Value::as_str);
    let args: Vec<&str> = found
        .get("args")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    command == Some(riabuild.display().to_string().as_str()) && args == arguments()
}

/// Whether an entry is riabuild's at all, whatever version wrote it.
///
/// Deliberately not "does the path match": that is [`is_current`], and asking it
/// here would make every upgraded riabuild treat its predecessor's entry as a
/// developer's own and refuse to repair it.
fn is_ours(found: &Value) -> bool {
    let args: Vec<&str> = found
        .get("args")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    args.starts_with(&["internal", "mcp-codex"])
}

/// What one account's config says about this checkout's `codex` server.
enum Found {
    /// No entry at all, or a project entry that does not exist yet.
    Missing,
    /// riabuild's, and current.
    Current,
    /// riabuild's, written by another version or with other flags.
    Stale,
    /// A developer's own. Left alone.
    Theirs,
}

fn look(root: &Map<String, Value>, key: &str, riabuild: &std::path::Path) -> Found {
    let found = root
        .get("projects")
        .and_then(Value::as_object)
        .and_then(|projects| projects.get(key))
        .and_then(Value::as_object)
        .and_then(|entry| entry.get("mcpServers"))
        .and_then(Value::as_object)
        .and_then(|servers| servers.get(NAME));
    match found {
        None => Found::Missing,
        Some(found) if is_current(found, riabuild) => Found::Current,
        Some(found) if is_ours(found) => Found::Stale,
        Some(_) => Found::Theirs,
    }
}

#[async_trait]
impl Task for ClaudeCodexMcp {
    fn id(&self) -> TaskId {
        "claude_codex_mcp"
    }

    fn title(&self) -> &str {
        "Codex subagents for Claude Code"
    }

    fn version(&self) -> u32 {
        1
    }

    fn depends_on(&self) -> &[TaskId] {
        // `claude_accounts` supplies the config directories to write into, one
        // per account; `project` supplies the checkout the entry is keyed
        // under, and a checkout moved by `project` is re-keyed at its new path.
        //
        // Deliberately *not* `codex_cli`. The entry names riabuild, not Codex,
        // and the Codex binary is resolved when a tool is called rather than
        // when this is written — so a machine whose Codex arrives later has a
        // correct entry in the meantime, and an upgraded Codex is not a reason
        // to rewrite a file Claude Code may be reading.
        &["claude_accounts", "project"]
    }

    /// The per-account `.claude.json`, which four sibling tasks also write.
    /// See `Task::writes`.
    fn writes(&self) -> &[Resource] {
        &["claude_config"]
    }

    async fn check(&self, ctx: &Ctx) -> Result<Status> {
        let Some(dir) = ctx.project_dir() else {
            return Ok(Status::needs("no project directory yet"));
        };
        if ctx.config.claude_accounts.is_empty() {
            return Ok(Status::needs("no Claude Code account yet"));
        }
        let riabuild = super::shims::running_binary()?;
        let keys = super::claude_trust::trust_keys(&dir).await;

        for (index, id) in ctx.config.claude_accounts.iter().enumerate() {
            let number = index + 1;
            let root = match claude_config::read(ctx, id).await {
                Stored::Missing => {
                    return Ok(Status::needs(format!(
                        "account {number} has no Claude Code config yet"
                    )));
                }
                Stored::Unreadable => {
                    return Ok(Status::needs(format!(
                        "the Claude Code config for account {number} is not valid JSON"
                    )));
                }
                Stored::Present(root) => root,
            };
            for key in &keys {
                match look(&root, key, &riabuild) {
                    Found::Current | Found::Theirs => {}
                    Found::Missing => {
                        return Ok(Status::needs(format!(
                            "account {number} cannot delegate to Codex yet"
                        )));
                    }
                    Found::Stale => {
                        return Ok(Status::needs(format!(
                            "account {number} points at a riabuild that is no longer running"
                        )));
                    }
                }
            }
        }
        Ok(Status::Satisfied)
    }

    async fn apply(&self, ctx: &mut Ctx) -> Result<()> {
        let Some(dir) = ctx.project_dir() else {
            return Err(Failure::new(
                "offering Codex to Claude Code as a subagent",
                "Run `riabuild` again — the checkout has to exist first.",
            )
            .into());
        };
        if ctx.config.claude_accounts.is_empty() {
            return Err(Failure::new(
                "offering Codex to Claude Code as a subagent",
                "Run `riabuild` again — a Claude Code account has to exist first.",
            )
            .into());
        }
        let riabuild = super::shims::running_binary()?;
        let keys = super::claude_trust::trust_keys(&dir).await;

        for id in ctx.config.claude_accounts.clone() {
            claude_config::edit(ctx, &id, |root| write_entry(root, &keys, &riabuild)).await?;
        }
        Ok(())
    }
}

/// Puts the entry into one account's config under every spelling of the
/// checkout, preserving everything it does not own.
///
/// Split out of `apply` because `claude_config::edit` takes a closure that
/// cannot be `async`, and because this is the whole of what the task changes —
/// a function that can be read on its own and tested without a config file.
fn write_entry(root: &mut Map<String, Value>, keys: &[String], riabuild: &std::path::Path) {
    let mut projects = match root.remove("projects") {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };

    for key in keys {
        // A checkout with no entry at all gets the shape Claude Code gives one
        // it creates itself, rather than a stub with a single key in it — the
        // same reasoning, and the same function, as `claude_trust`.
        let project = projects
            .entry(key.clone())
            .or_insert_with(super::claude_trust::new_project_entry);
        let Some(project) = project.as_object_mut() else {
            continue;
        };
        let servers = project
            .entry("mcpServers".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(servers) = servers.as_object_mut() else {
            continue;
        };
        // Stands aside from a `codex` server the developer configured
        // themselves, and repairs one an older riabuild wrote.
        match servers.get(NAME) {
            Some(found) if !is_ours(found) => continue,
            _ => {
                servers.insert(NAME.to_string(), entry(riabuild));
            }
        }
    }
    root.insert("projects".to_string(), Value::Object(projects));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn riabuild() -> PathBuf {
        PathBuf::from("/home/ada/.riabuild/riabuild/2026.09.04/riabuild")
    }

    fn written() -> Map<String, Value> {
        let mut root = Map::new();
        write_entry(&mut root, &["/work/hub".to_string()], &riabuild());
        root
    }

    fn server_in(root: &Map<String, Value>) -> Value {
        root["projects"]["/work/hub"]["mcpServers"]["codex"].clone()
    }

    #[test]
    fn the_entry_names_riabuild_and_nothing_a_server_chose() {
        let server = server_in(&written());
        assert_eq!(server["type"], "stdio");
        assert_eq!(server["command"], riabuild().display().to_string());
        assert_eq!(server["args"][0], "internal");
        assert_eq!(server["args"][1], "mcp-codex");
    }

    #[test]
    fn a_checkout_with_no_entry_gets_a_whole_project_entry() {
        let root = written();
        // The trust key is what a first `riabuild` writes beside this one, and
        // a project entry missing it would put the dialog back.
        assert_eq!(
            root["projects"]["/work/hub"]["hasTrustDialogAccepted"],
            true
        );
    }

    #[test]
    fn writing_twice_changes_nothing() {
        let mut root = written();
        write_entry(&mut root, &["/work/hub".to_string()], &riabuild());
        assert_eq!(root, written());
    }

    #[test]
    fn an_entry_this_riabuild_wrote_is_current() {
        let root = written();
        assert!(matches!(
            look(&root, "/work/hub", &riabuild()),
            Found::Current
        ));
    }

    #[test]
    fn an_older_riabuilds_entry_is_stale_rather_than_someone_elses() {
        let mut root = Map::new();
        write_entry(
            &mut root,
            &["/work/hub".to_string()],
            &PathBuf::from("/home/ada/.riabuild/riabuild/2026.08.01/riabuild"),
        );
        assert!(matches!(
            look(&root, "/work/hub", &riabuild()),
            Found::Stale
        ));

        // And repairing it is what `apply` does with it.
        write_entry(&mut root, &["/work/hub".to_string()], &riabuild());
        assert!(matches!(
            look(&root, "/work/hub", &riabuild()),
            Found::Current
        ));
    }

    #[test]
    fn a_developers_own_codex_server_is_left_alone() {
        let mut root = Map::new();
        root.insert(
            "projects".to_string(),
            json!({
                "/work/hub": {
                    "mcpServers": {
                        "codex": { "type": "stdio", "command": "codex", "args": ["mcp-server"] }
                    }
                }
            }),
        );
        assert!(matches!(
            look(&root, "/work/hub", &riabuild()),
            Found::Theirs
        ));

        write_entry(&mut root, &["/work/hub".to_string()], &riabuild());
        assert_eq!(server_in(&root)["command"], "codex");
        assert_eq!(server_in(&root)["args"][0], "mcp-server");
    }

    #[test]
    fn another_servers_entry_is_untouched() {
        let mut root = Map::new();
        root.insert(
            "projects".to_string(),
            json!({ "/work/hub": { "mcpServers": { "sentry": { "command": "sentry-mcp" } } } }),
        );
        write_entry(&mut root, &["/work/hub".to_string()], &riabuild());
        assert_eq!(
            root["projects"]["/work/hub"]["mcpServers"]["sentry"]["command"],
            "sentry-mcp"
        );
        assert_eq!(server_in(&root)["args"][1], "mcp-codex");
    }

    #[test]
    fn a_missing_entry_is_missing_rather_than_current() {
        assert!(matches!(
            look(&Map::new(), "/work/hub", &riabuild()),
            Found::Missing
        ));
    }
}
