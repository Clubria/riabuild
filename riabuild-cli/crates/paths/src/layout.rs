//! Every path riabuild knows, as one trait.
//!
//! Behind a trait rather than free functions so a test can point the whole tree
//! at a tempdir, and so the two roots below stay a property of the
//! implementation rather than something each call site re-derives. Note which
//! methods hang off `tools_root()` rather than `root()`: on a laptop they are
//! the same directory and either spelling works, and on a server they are not.

use std::path::PathBuf;

pub trait Paths: Send + Sync {
    /// This developer's own state. `~/.riabuild` on a laptop; on a shared
    /// server it is namespaced per developer — see [`root_for`](crate::root_for).
    fn root(&self) -> PathBuf;
    /// The developer's real home, for locating their own shell rcfiles.
    fn home(&self) -> PathBuf;

    /// Tools everyone on this machine shares: node, pnpm, gh, infisical, and
    /// riabuild itself. Equal to `root()` on a laptop; on a server it stays at
    /// `~/.riabuild` while `root()` moves into a per-developer namespace, so one
    /// Unix account holds one toolchain and several developers.
    fn tools_root(&self) -> PathBuf {
        self.root()
    }

    fn state_file(&self) -> PathBuf {
        self.root().join("state.json")
    }
    fn config_file(&self) -> PathBuf {
        self.root().join("config.json")
    }
    /// Guards a read-modify-write of `state.json`, `config.json` or
    /// `remotes.json`. Held for milliseconds.
    ///
    /// Deliberately none of those files. Writes land by `rename`, so a lock
    /// taken on the data file would be a lock on an inode the next write
    /// unlinks — the following process would lock a fresh inode, see no
    /// contention, and proceed. A lock's identity has to outlive the data it
    /// guards, and that failure is invisible to every single-process test.
    fn state_lock_file(&self) -> PathBuf {
        self.root().join(".state.lock")
    }
    /// Guards the provisioning phase, so two runs do not install one toolchain
    /// twice. Held for seconds to minutes, and never across the shell handoff.
    ///
    /// Separate from `state_lock_file` because a run holding this one saves
    /// state after every task, and `std` is explicit that a second lock taken
    /// by a process that already holds one is unspecified and may deadlock.
    fn provision_lock_file(&self) -> PathBuf {
        self.root().join(".provision.lock")
    }
    fn org_settings_file(&self) -> PathBuf {
        self.root().join("org-settings.json")
    }
    /// This riabuild's own session, when it goes in a file rather than a
    /// keyring. Two machines reach it, and the root is what tells them apart: a
    /// **managed server**, where the root is the developer's namespace under
    /// `.riabuild-remote/<member-id>`, and a **headless machine** with no
    /// Secret Service answering, where it is the ordinary `~/.riabuild`.
    ///
    /// Not used where there *is* a keyring — see `keychain::select`, which owns
    /// the decision, and `keychain::keyring_answers`, which owns the question
    /// the second case turns on.
    fn session_token_file(&self) -> PathBuf {
        self.root().join("session.token")
    }
    /// Who this namespace belongs to, in words, for whoever has a shell on the
    /// box and finds a directory named after a UUID.
    fn owner_file(&self) -> PathBuf {
        self.root().join("owner.json")
    }
    fn bin_dir(&self) -> PathBuf {
        self.root().join("bin")
    }
    /// The Claude Code status line script. Not in `bin/`, because it is a Node
    /// script Claude Code runs by name rather than something that belongs on
    /// `PATH`.
    ///
    /// **`tools_root()`, not `root()`** — the one thing about this path that is
    /// load-bearing. The org settings name it as `node
    /// ~/.riabuild/claude-statusline.js`, Claude Code runs that through a shell,
    /// and `~` is the account's home on every machine. On a laptop the two roots
    /// coincide and either spelling works; on a server `root()` moves into
    /// `~/.riabuild-remote/<member-id>` and only `tools_root()` stays where the
    /// command points. Built on `root()`, as it was until 2026-08-17, the script
    /// landed in the namespace, `node` was handed a path that did not exist, and
    /// a status line whose command fails is simply absent — so remote mode had
    /// none, with the task reporting satisfied throughout.
    ///
    /// It belongs in the shared tree on its own merits, which is why the fix is
    /// this way round rather than a path rewritten per developer: the bytes are
    /// compiled into the binary, so every developer on the box gets the same
    /// ones, and this is a *program the machine runs* — the same category as the
    /// node that runs it. Sharing it costs one write when two developers on one
    /// server run different riabuilds, which `claude_statusline`'s byte
    /// comparison already treats as ordinary drift.
    fn claude_statusline_file(&self) -> PathBuf {
        self.tools_root().join("claude-statusline.js")
    }
    fn node_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("node").join(version)
    }
    /// pnpm 11 and newer are a launcher plus the `dist/` tree it loads, so they
    /// get a directory of their own rather than a file in `bin/`.
    fn pnpm_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("pnpm").join(version)
    }
    fn riabuild_dir(&self, version: &str) -> PathBuf {
        self.tools_root().join("riabuild").join(version)
    }
    /// `~/.riabuild/<tool>/<version>` — an owned copy of a third-party CLI.
    ///
    /// Versioned, so bumping a pin installs beside the old copy rather than
    /// writing over a binary that may be running.
    fn tool_dir(&self, tool: &str, version: &str) -> PathBuf {
        self.root().join(tool).join(version)
    }
    /// Where `riabuild agents` keeps its sessions.
    ///
    /// Under `root()` rather than `tools_root()`, which is the whole of what
    /// makes two developers on one server invisible to each other: `root()` is
    /// the per-developer namespace, and a session record names a checkout, a
    /// thread id and a transcript that belong to one person.
    fn agents_dir(&self) -> PathBuf {
        self.root().join("agents")
    }
    /// One session's directory: its record, its spool, and its lock.
    fn agent_session_dir(&self, id: &str) -> PathBuf {
        self.agents_dir().join(id)
    }
    fn claude_dir(&self) -> PathBuf {
        self.root().join("claude")
    }
    /// One developer's Claude Code profile — what `CLAUDE_CONFIG_DIR` points at.
    fn claude_profile_dir(&self, profile: &str) -> PathBuf {
        self.claude_dir().join(profile)
    }
    /// Claude Code's own state for that profile. Named by Claude Code, not by
    /// riabuild: it puts `.claude.json` inside whatever `CLAUDE_CONFIG_DIR` is.
    fn claude_config_file(&self, profile: &str) -> PathBuf {
        self.claude_profile_dir(profile).join(".claude.json")
    }
    /// Guards a read-modify-write of one account's `.claude.json`.
    ///
    /// Per account, not per run: two accounts' configs are two files, and one
    /// lock over both would serialise edits that never meet.
    ///
    /// Not `claude_config_file` itself, for [`state_lock_file`]'s reason — that
    /// write lands by `rename`, so a lock on the data file is a lock on an
    /// inode the next write unlinks. And inside the profile directory rather
    /// than beside it, so that `riabuild claude remove` takes it away with the
    /// account: a lock in `claude_dir()` would outlive every account it was
    /// ever made for, and nothing sweeps that directory.
    ///
    /// [`state_lock_file`]: Paths::state_lock_file
    fn claude_config_lock_file(&self, profile: &str) -> PathBuf {
        self.claude_profile_dir(profile).join(".claude.json.lock")
    }
    /// The nine Codex profiles, one directory each.
    ///
    /// A parent rather than a `CODEX_HOME` itself. Codex keeps its credentials
    /// in `$CODEX_HOME/auth.json` and nowhere else — no OS keychain is
    /// involved — so two homes really are two independent sign-ins, in the way
    /// `claude_profile_dir` is for Claude Code. Verified against Codex 0.147.0:
    /// two homes hold two different API keys at once, and `codex logout` in one
    /// leaves the other logged in.
    fn codex_dir(&self) -> PathBuf {
        self.root().join("codex")
    }
    /// One Codex profile — what `CODEX_HOME` points at.
    ///
    /// Numbered `1`..=`9` rather than named by a uuid the way Claude Code's
    /// are. Claude's accounts are created by riabuild's own sign-in flow and
    /// can be deleted and renumbered, so their *position* in a list is their
    /// number and the directory name has to survive that. Codex profiles are a
    /// fixed set riabuild creates once and never reorders, so the directory
    /// name can simply be the number — which makes `codex-3` and
    /// `~/.riabuild/codex/3` obviously the same thing to anyone reading their
    /// own disk.
    ///
    /// It has to *exist*: Codex refuses to start against a `CODEX_HOME` that is
    /// not there ("Error finding codex home"), so naming one is only half of
    /// pointing at it. `codex_cli` creates all nine, and each generated
    /// launcher recreates its own.
    fn codex_profile_dir(&self, profile: usize) -> PathBuf {
        self.codex_dir().join(profile.to_string())
    }
    /// The nine Grok Build profiles, one directory each.
    ///
    /// The same shape as `codex_dir`, and for the same verified reason: Grok
    /// Build keeps its credentials in `$GROK_HOME/auth.json` and nowhere else —
    /// no OS keychain — so two homes really are two independent sign-ins.
    /// `GROK_HOME` also carries the rest of that account's local state:
    /// `config.toml`, sessions, MCP registrations, hooks and plugins. Verified
    /// against Grok Build 1.0.5.
    fn grok_dir(&self) -> PathBuf {
        self.root().join("grok")
    }
    /// One Grok Build profile — what `GROK_HOME` points at.
    ///
    /// Numbered `1`..=`9` for the reason `codex_profile_dir` is: the set is
    /// fixed, nothing is ever created or renumbered, so `grok-3` and
    /// `~/.riabuild/grok/3` are obviously the same thing to anyone reading
    /// their own disk. Claude Code's uuids exist because its accounts *can* be
    /// deleted and renumbered, and position is then the account number.
    ///
    /// Unlike a `CODEX_HOME`, this one does **not** have to exist first: Grok
    /// Build creates a `GROK_HOME` that is not there rather than refusing to
    /// start, verified against 1.0.5. riabuild creates all nine anyway, so that
    /// "nine accounts" is a state of the machine `check()` can assert rather
    /// than a promise that comes true the first time each launcher is run.
    fn grok_profile_dir(&self, profile: usize) -> PathBuf {
        self.grok_dir().join(profile.to_string())
    }
    fn shell_dir(&self, shell: &str) -> PathBuf {
        self.root().join("shell").join(shell)
    }
    fn log_file(&self) -> PathBuf {
        self.root().join("logs").join("riabuild.log")
    }
    /// Usage samples the status line has written and nothing has sent yet.
    ///
    /// Under `root()` and emphatically not `tools_root()`, unlike the status
    /// line script that writes into it. The script is a program the machine
    /// runs and every developer on a server gets the same bytes; a sample names
    /// one person's session, and putting it in the shared tree would let two
    /// developers on one box read each other's.
    ///
    /// That split is the reason the script derives this path from
    /// `CLAUDE_CONFIG_DIR` rather than having it compiled in: the script is one
    /// constant on every machine, and `<root>/claude/<uuid>` is the only thing
    /// in its environment that names the per-developer root.
    fn usage_dir(&self) -> PathBuf {
        self.root().join("usage")
    }
    /// One Claude account's spool. The file name *is* the account id, which is
    /// what lets the status line name the account without being told it twice.
    fn usage_spool_file(&self, account: &str) -> PathBuf {
        self.usage_dir().join(format!("{account}.ndjson"))
    }
    /// Held for the length of a flush, and taken non-blocking.
    ///
    /// Three Claude Code windows on one laptop notice a stale spool in the same
    /// second, and the one that wins is doing the work that makes the other two
    /// unnecessary — so they exit rather than queue. An `flock` and never a pid
    /// file, for the reason in the many-windows spec: the kernel releases it
    /// however the process ends.
    fn usage_lock_file(&self) -> PathBuf {
        self.usage_dir().join("flush.lock")
    }
    /// Touched when a flush is *attempted*, which is what paces the status
    /// line's one-a-minute check.
    ///
    /// Attempted rather than succeeded, deliberately. A laptop that cannot
    /// reach riabuild-web should retry every minute and no more often; a marker
    /// that only moved on success would make an unreachable dashboard into a
    /// spawned process on every render.
    fn usage_flushed_marker(&self) -> PathBuf {
        self.usage_dir().join("flushed")
    }
    /// The servers this laptop knows about — see `remote::store`.
    fn remotes_file(&self) -> PathBuf {
        self.root().join("remotes.json")
    }
    /// The private key riabuild makes for each server, one file per
    /// `Remote::hash()`. Never shared with anything else riabuild writes;
    /// see `remote::identity`.
    fn identity_dir(&self) -> PathBuf {
        self.root().join("ssh-identities")
    }
    /// Where the `ssh-agent` riabuild runs for one server keeps its socket and
    /// the public halves that address its identities.
    ///
    /// One directory per server, so two `riabuild remote` runs against
    /// different machines cannot take each other's socket over.
    ///
    /// Note what does **not** live here, and cannot: the issued private keys
    /// themselves. They go from the API response into `ssh-add`'s stdin and
    /// exist nowhere on a filesystem. A socket and a public key are both inert
    /// — see `remote::issued::agent`.
    fn agent_dir(&self, server_hash: &str) -> PathBuf {
        self.root().join("agent").join(server_hash)
    }
    /// Where riabuild's own `known_hosts` lives — never the developer's
    /// `~/.ssh/known_hosts`. See `remote::identity::ssh_options`'s `-F
    /// /dev/null`, which is what makes that true.
    fn ssh_dir(&self) -> PathBuf {
        self.root().join("ssh")
    }
    fn known_hosts_file(&self) -> PathBuf {
        self.ssh_dir().join("known_hosts")
    }
    /// The `SSH_ASKPASS` helper riabuild points `ssh` at, so a password for a
    /// server is asked for once rather than at every one of the connections a
    /// single `riabuild remote` opens. Written on every run — see
    /// `remote::askpass::ensure_helper`.
    fn askpass_helper(&self) -> PathBuf {
        self.ssh_dir().join("askpass")
    }
    /// Where a saved SSH password lands on a machine with **no keyring at
    /// all**. The keychain is preferred everywhere it exists; see
    /// `keychain::select_password_store`, which owns that decision, and the
    /// amended "No secrets in `~/.riabuild/`" note in `CLAUDE.md`.
    fn remote_password_file(&self, hash: &str) -> PathBuf {
        self.ssh_dir().join("passwords").join(hash)
    }
    /// The laptop's cache of one *server's* session token, on a laptop with no
    /// keyring to hold it. Keyed by `Remote::hash()`, so several servers never
    /// collide and `remote forget` deletes exactly one.
    ///
    /// Separate from [`remote_password_file`](Paths::remote_password_file)
    /// because they are different secrets for the same server — a riabuild
    /// bearer token and a Unix password — and `forget` deletes them
    /// individually. Separate from
    /// [`session_token_file`](Paths::session_token_file) because that one is
    /// *this* machine's own session: one file per laptop, not per server.
    fn remote_session_file(&self, hash: &str) -> PathBuf {
        self.root().join("remote-sessions").join(hash)
    }
    /// Which of this laptop's windows gets to decide whether a server needs a
    /// new riabuild session.
    ///
    /// One person opening two terminals into one server is the ordinary way
    /// remote mode is used, and `session::ensure` is a read (*is the saved
    /// token still good?*) and a write (*mint one*) with a network round trip
    /// in between. Run twice at once against a server whose token has expired,
    /// both windows mint — and the second one's `session_id` overwrites the
    /// first's on the record, which leaves a live 90-day session on riabuild-web
    /// that no `riabuild remote forget` can ever name. That is the one state
    /// `session.rs` says out loud it must never produce.
    ///
    /// A lock of its own rather than `state_lock_file`, which the store's own
    /// `persist_one` takes from inside this one: `std` is explicit that a
    /// second lock taken by a process already holding one is unspecified.
    /// Sibling of [`remote_session_file`](Paths::remote_session_file) and not
    /// that file itself, for the reason
    /// [`state_lock_file`](Paths::state_lock_file) sets out — a lock on a file
    /// something renames over is a lock on an inode the next writer never sees.
    fn remote_session_lock_file(&self, hash: &str) -> PathBuf {
        self.root()
            .join("remote-sessions")
            .join(format!("{hash}.lock"))
    }
}
