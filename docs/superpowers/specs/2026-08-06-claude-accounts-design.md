# Claude Code account management

**Date:** 2026-08-06
**Status:** Implemented — shipped in #28 on 2026-08-07; the "not yet implemented" this
line used to carry was true only for the day between
**Supersedes:** task 7 (`claude_profiles`) in
[`2026-08-04-riabuild-design.md`](2026-08-04-riabuild-design.md)

Written against `main` at `70f52c2`.

A Clubria developer runs more than one Claude Code account — a personal subscription and
one or more work accounts, or two subscriptions to spread usage limits. Today riabuild
provisions exactly one profile directory and one launcher named `c`. Switching accounts
means logging out and back in, which destroys the session history of the account being
left.

This spec replaces the single profile with an ordered list of accounts, each its own
`CLAUDE_CONFIG_DIR`, each launched by its own command, all of them under the org's Claude
settings and all of them trusting the checkout.

## What the developer sees

`claude` runs the primary account. `claude-1` … `claude-9` run accounts by number. Every
riabuild shell opens with the list:

```
Your Claude Code accounts:

  1. claude-1 / claude   clubria@proton.me
  2. claude-2            other@gmail.com
  3. claude-3            (logged out)

  Add an account:     riabuild claude new
  Delete an account:  riabuild claude delete 3
  Make it primary:    riabuild claude primary 2
  Log in:             claude-3 auth login
```

Hints appear only when they would work. With one account, `delete` refuses and `primary`
is meaningless, so neither line is shown; the `Log in:` line appears only while some
account is logged out; `Add an account:` disappears at nine. Every line on screen is a
command that succeeds right now.

Deleting account 3 makes account 4 into account 3. The developer never types a UUID.

## What Claude Code actually guarantees

Three facts, verified against Claude Code 2.1.223, that the design rests on. None are in
the public settings documentation, so all three are pinned by tests (see *Testing*).

**`CLAUDE_CONFIG_DIR` scopes credentials, not just configuration.** The macOS keychain
service name is built as:

```js
`Claude Code${OAUTH_FILE_SUFFIX}-credentials${suffix}`
// suffix = ""                             when CLAUDE_CONFIG_DIR is unset
//        = "-" + sha256(configDir)[0..8]  when it is set
```

and the file fallback is `<configDir>/.credentials.json`. Two accounts in two directories
therefore hold two independent logins. This is what makes the whole feature possible.

It also means **the config directory's path string is the account's identity**. Moving a
directory orphans its login; removing a directory before logging out leaves a keychain
item nothing can ever reach again. Hence the deletion order in `riabuild claude delete`.

**`claude auth status --json` reports identity per config directory.** It prints
`{"loggedIn": true, "authMethod": "claude.ai", "email": "…", "orgName": "…"}` and exits
**0** when logged in, **1** when logged out. Pointed at a fresh directory it reports
`loggedIn: false`. This is a supported CLI surface, so riabuild reads identity from it
rather than parsing undocumented keys out of `.claude.json`.

riabuild reads the `loggedIn` field and **not** the exit code. Both say the same thing
today, but an exit code is a single channel that a future release could also use for
"could not reach the server", and conflating that with "signed out" would have the box
state something false. Non-zero is never a `Failure` here.

**A global `--settings` is accepted ahead of a subcommand.** `claude --settings <file>
auth status` works. Every shim passes `--settings` unconditionally, so `claude-3 auth
login` — which the box tells developers to run — depends on this.

**It costs about 450 ms per call**, almost all CPU. That is why the account list is
gathered concurrently rather than serially.

## Data model

`UserConfig` gains one field, and demotes another to legacy:

```rust
/// Claude Code config directories, in the order the developer numbers them.
/// Position is the number; index 0 is the primary account. UUIDs are the only
/// identity anything persists.
#[serde(default)]
pub claude_accounts: Vec<String>,

/// The single profile older riabuilds recorded. Folded into `claude_accounts`
/// when the config is loaded, and never written back.
#[serde(default, skip_serializing)]
pub claude_profile: Option<String>,
```

Position *is* the number. `delete N` is `Vec::remove(N - 1)` and every later account's
number shifts on its own; `primary N` is a remove-and-insert-at-0. A design that stored an
explicit number would have an invariant to maintain on every mutation, and would
eventually fail to.

The list is capped at 9, so shim names stay single-digit and `riabuild claude delete 12`
is rejected as obviously wrong rather than interpreted.

The disk layout does not change: accounts remain `~/.riabuild/claude/<uuid>/`, and
`paths.rs` needs no new entries — `claude_profile_dir` and `claude_config_file` already
take an id. What changes is that riabuild now expects more than one.

### Migration

`UserConfig::load` folds the legacy field in, so no caller ever sees it:

| On disk | Loaded as |
|---|---|
| `claude_accounts: ["a", "b"]` | `["a", "b"]` |
| `claude_profile: "a"`, no `claude_accounts` | `["a"]` |
| both | `claude_accounts` wins |
| neither | `[]` — the task creates account 1 |

`skip_serializing` makes the field disappear from `config.json` on the next save. The
stale `claude_profiles` key is dropped from `state.json` at the same point, so a renamed
task does not leave a dead record behind forever.

## Reading the accounts — `accounts/status.rs`

```rust
pub struct Account {
    pub number: usize,      // 1-based, derived from position — never stored
    pub id: String,         // the UUID directory name
    pub identity: Identity,
}

pub enum Identity {
    LoggedIn(String),       // email
    LoggedOut,
    Unknown(String),        // why we cannot tell
}
```

One `claude auth status --json` per account, each with its own `CLAUDE_CONFIG_DIR`, all
spawned into a `tokio::task::JoinSet` and joined. Wall clock is one call, not N.

The runtime is current-thread by construction (`Cargo.toml` takes `rt` without
`rt-multi-thread`), and that is fine: the 450 ms is the child process's CPU, not
riabuild's. Every task blocks on a subprocess it is awaiting, so the reactor interleaves
them and all N children run at once.

`Unknown` is a distinct state on purpose. `loggedIn: false` means Claude Code answered
"signed out"; a missing binary or unparseable output means riabuild does not know, and
rendering that as `(logged out)` would assert a fact it does not have. `github_cli.rs`
documents the same distinction at length — a captive portal must not read as "you were
removed from the org".

The box renders `Unknown` as `(cannot tell — <reason>)`, in one form everywhere it is
printed. A second, more verbose rendering would mean two things to keep in agreement.

## Finding the binary — `Ctx::claude()`

Every call to Claude Code goes through an absolute path, exactly as `ctx.gh()` and
`ctx.infisical()` already do:

```rust
/// The Claude Code riabuild installed, by absolute path.
pub fn claude(&self) -> String
```

`~/.riabuild/node/<pinned>/bin/claude` when a Node version is pinned, and the bare name
otherwise, which is all a machine with no toolchain yet could use. This replaces the
`runner.which("claude")` in today's `claude_profiles`, which reads the ambient `PATH` —
during provisioning that does not contain riabuild's Node, so it finds whatever the
developer happens to have installed, or nothing at all just after riabuild installed one.

## Shims — `shims/mod.rs`

`c` is removed. It is deleted from `bin/` on the next run, and from the README and the v1
design spec.

Generated instead, from a single `launcher_script`:

| Script | Account |
|---|---|
| `claude` | index 0 |
| `claude-1` … `claude-N` | by position |

Each script is complete rather than delegating to a shared helper: the deduplication
belongs in the Rust function that generates them, where it is testable, not in a second
layer of shell.

```sh
#!/bin/sh
# Generated by riabuild. Edits here are overwritten.
set -e
CLAUDE_CONFIG_DIR="/Users/ada/.riabuild/claude/11111111-2222-4333-8444-555555555555"
export CLAUDE_CONFIG_DIR
claude_binary="/Users/ada/.riabuild/node/22.23.1/bin/claude"
if [ ! -x "$claude_binary" ]; then
  # The recorded binary is gone — a `claude update` that migrated to a native
  # install, or a Node version change since the last run. Fall back to PATH with
  # riabuild's own bin/ removed, so this script cannot find itself.
  PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "/Users/ada/.riabuild/bin" | paste -sd: -)
  export PATH
  claude_binary=claude
fi
if [ -f "/Users/ada/.riabuild/org-settings.json" ]; then
  exec "$claude_binary" --settings "/Users/ada/.riabuild/org-settings.json" "$@"
fi
exec "$claude_binary" "$@"
```

**The absolute path is not an optimisation, it is the only correct form.**
`~/.riabuild/bin` is first on `PATH`, so a script named `claude` that runs `exec claude`
— which is what today's `c` does — finds itself and recurses until the shell runs out of
processes. The PATH-stripping fallback exists so that a `claude update` between provisions
cannot leave `claude` a dead command in the developer's shell, which would read as *Claude
Code is uninstalled*. `paste -sd: -` rather than `tr '\n' ':'`: the latter leaves a
trailing colon, and an empty `PATH` entry means the current directory.

Two properties fall out of the shim shape rather than needing code:

- **Org settings apply to every account**, because every shim passes
  `--settings org-settings.json`. Layering at launch was already the design; nothing about
  it needed to change for N accounts.
- **`claude-3 auth login`, `claude-2 --resume` work**, because every shim passes `"$@"`.

`write_all` prunes `bin/c` and any `claude-<n>` above the account count. An orphaned shim
is worse than a missing one: it points at a deleted directory, so Claude Code would create
it fresh, prompt for a login, and leave an unregistered account no riabuild command can
see.

### The `CLAUDE_CONFIG_DIR` export is removed

`shell/mod.rs` currently exports `CLAUDE_CONFIG_DIR` into the environment shell. With
shims that each set it themselves, that export only causes harm:

- `riabuild claude primary 2` rewrites the shims, which already-open shells pick up
  immediately — but a variable exported at spawn time cannot be updated, so the shell
  would disagree with itself about which account is primary.
- A `claude` started outside a shim — an IDE extension with a hardcoded binary path —
  would inherit the primary account's directory but *not* `--settings`, silently running
  a Clubria account without org policy. Without the export it lands on the developer's
  default config directory, which is at least obviously not riabuild's.

The shims become the single source of truth for both *which account* and *which settings*.

## Trust applies to every account — `tasks/claude_trust.rs`

`claude_trust` writes `hasTrustDialogAccepted` into a profile's `.claude.json`, keyed by
the checkout's absolute path. It is per-config-directory state that no settings file can
express, which is exactly why the task exists — and exactly why it cannot stay
single-account. An untrusted `claude-2` opens a modal on first launch and holds back the
org's settings as untrusted, which is the one dialog this task exists to prevent.

`check()` is satisfied only when **every** account trusts the checkout; `apply()` writes
the trust key into each account's config file. The existing read-modify-write, the
symlink-aware `trust_keys`, and the move-aside of an unreadable config are unchanged and
simply run once per account.

`claude_statusline` needs no such change: it installs one script at
`~/.riabuild/claude-statusline.js` that the org settings name by path, and every account
gets those settings through `--settings`.

## Commands — `accounts/command.rs`

```
riabuild claude                 the box
riabuild claude list            the box (alias, for people who expect a verb)
riabuild claude new             add an account and sign it in
riabuild claude delete N [--yes]  remove an account
riabuild claude primary N       make account N the primary
```

Bare `claude` and `list` print identical output. All of them work outside the riabuild
shell, resolving the real binary through `ctx.claude()` the same way the shims do.

### `riabuild claude new`

Refuse past 9 → generate a UUID → create the directory → rewrite the shims → run
`claude auth login` interactively with `CLAUDE_CONFIG_DIR` set → **re-run
`claude auth status`** → print the box.

The re-check is the point. If the login did not take — browser closed, wrong account,
`auth login` exiting non-zero — the directory is removed and riabuild reports that no
account was added. An abandoned `new` leaves nothing behind. This is the engine's
"`apply()` is always followed by a re-run of `check()`" rule applied to a command; the
alternative is a registered account permanently showing `(logged out)` because of a
browser tab someone closed.

`new` does not open a Claude Code session. Signing in is the whole job; the developer
starts the session with `claude-3` when they want one.

### `riabuild claude delete N`

```
$ riabuild claude delete 3

  Delete account 3 — third@gmail.com?
  Its Claude Code sessions, history and login are removed.
  [y/N] y

  ● Signed out third@gmail.com
  ● Removed ~/.riabuild/claude/9f2c…8a1
  ● Account 4 is now account 3
```

The confirmation follows `reset.rs` exactly, because it is the same kind of irreversible
delete: a `--yes` flag skips it; `!ui.interactive()` is a `Failure` naming `--yes` rather
than an assumed answer; the question is `ui.ask("… [y/N]")` and only `y`/`yes` proceeds.
`Ui::confirm` is not used — it defaults to **yes**, which is right for "shall I install
this" and wrong for a delete. An empty answer must decline.

The confirmation names the email, not just the number, because renumbering means a
mistyped index destroys a different account's history than intended, unrecoverably.

Order is load-bearing: `claude auth logout` runs **before** the directory is removed,
because the keychain item is keyed by `sha256(configDir)` and removing the directory first
orphans a credential permanently.

Deleting the only account is refused:

```
  riabuild stopped: deleting your only Claude Code account
    do this: add another with `riabuild claude new` first
```

Permitting it would leave a machine that the next `riabuild` run repairs by creating an
empty account and demanding a browser login — a worse outcome than refusing.

### `riabuild claude primary N`

Moves account N to index 0, rewrites the shims, prints the box. Without it, changing which
account `claude` runs would mean deleting and re-creating an account — throwing away its
session history to achieve a reordering.

## Provisioning — `tasks/claude_accounts.rs`

Replaces `tasks/claude_profiles.rs`. Task id becomes `claude_accounts`; `depends_on` stays
`["toolchain"]`; `claude_trust` depends on it under the new name.

`check()` reports `Needs` when:

| Condition | Reason shown |
|---|---|
| Claude Code is not installed | `Claude Code is not installed` |
| `claude --version` < `MIN_VERSION` | `Claude Code <found> is older than 2.1.223` |
| no accounts registered | `no Claude Code account yet` |
| a registered directory is missing | `Claude Code account <n>'s directory is missing (<id>)` |
| an unregistered UUID directory exists | `the Claude Code account directory <id> is not registered` |
| account 1 is signed out | `account 1 is not signed in` |
| account 1's status will not parse | `riabuild could not tell whether account 1 is signed in: <why>` |

`MIN_VERSION` is `2.1.223` and must not be lowered: the three behaviours above were only
ever verified against that build, a 2.0.x machine passed the original `2.0.0` floor while
possibly lacking `auth status --json` altogether, and the floor costs nothing to hold —
`install_claude` runs `npm install -g @anthropic-ai/claude-code` unpinned, so anyone below
it is upgraded to latest rather than blocked. The reason names the version *found*, because
"older than 2.1.223" without it leaves a developer unable to tell whether the upgrade
worked.

The last row is why `Identity` has three states: a status riabuild could not read is
reported as unreadable, never as signed out. Claiming a developer is signed out when the
network was down would send them to a browser to fix nothing.

`apply()`:

1. Install Claude Code with riabuild's npm if missing (unchanged).
2. Drop registered accounts whose directories are gone.
3. **Adopt** unregistered UUID directories, oldest first — by creation time (APFS
   `birthtime`, `statx` `btime`), falling back to `mtime` where the filesystem has none.
   `mtime` alone would let a recently used account adopt as account 1.
4. Create account 1 if the list is empty.
5. Run `claude auth login` interactively for account 1 if it is signed out, and hard-error
   if that is abandoned.
6. Save the config and rewrite the shims.

Step 3 is what rescues a developer whose `config.json` was lost while their profile
directory, sessions and login are all still on disk. Discovering that state and ignoring
it would strand real work behind a directory riabuild refuses to name.

Step 5 follows `github_cli.rs`: an interactive browser sign-in inside `apply()`, with the
exit code checked, because riabuild's job is "running Claude Code against our codebase"
and a signed-out Claude Code is not that. Accounts 2 through 9 are never blocking —
`check()` ignores their login state and the box reports it.

Only account 1's status is read during `check()`, so the added cost is one ~450 ms call in
a phase that already makes network round trips.

## Where the box prints

Into the generated rcfile, alongside the banner. `shell::prelude` renders the box followed
by the banner, and each shell wraps it in the `banner_command` it already has — so the
`[[ -t 1 ]]` guard that keeps the banner out of captured output covers the box too, and
the "printed once, after the developer's own config" property is inherited rather than
re-derived. `Shell::Other` has no rcfile, and its branch in `spawn` prints the prelude
itself, exactly as it already does for the banner.

`riabuild claude new`, `delete`, `primary` and `list` print the box on stdout.

## Layout

```
src/accounts/mod.rs      registry: order, add, remove, promote, ids, cap of 9
src/accounts/status.rs   concurrent `claude auth status` → Vec<Account>
src/accounts/render.rs   the box
src/accounts/command.rs  riabuild claude new|delete|primary|list
src/shims/mod.rs         claude + claude-1..N, prune c and stale numbers
src/tasks/claude_accounts.rs   replaces claude_profiles.rs
src/tasks/claude_trust.rs      every account, not just one
src/cli.rs               Command::Claude subcommand
src/shell/{mod,zsh,bash,fish}.rs   prelude carries the box
src/config.rs            claude_accounts, legacy fold
src/tasks/mod.rs         Ctx::claude()
src/runner.rs            FakeRunner matches declared env pairs
src/reset.rs             warns about N accounts, not one profile
```

`accounts/mod.rs` owns UUID generation and the id predicate, both moved out of
`claude_profiles.rs`, so the task file is about machine state and the account module is
about the registry.

## Testing

**`FakeRunner` must match on environment.** `claude auth status --json` is the same command
string for every account; only `CLAUDE_CONFIG_DIR` differs, and `FakeRunner::run` ignores
`RunOptions.env` entirely. Until it does not, the test *"account 1 is signed in, account 2
is signed out"* cannot be written — which is the central behaviour of this feature. Add:

```rust
pub fn with_env(self, invocation: &str, env: &[(&str, &str)],
                code: i32, stdout: &str, stderr: &str) -> Self
```

A stub matches when the invocation matches *and* every declared env pair is present;
longest invocation wins, then most env pairs. Existing `with` stubs keep working as
env-agnostic matches. This is not incidental test plumbing: `CommandRunner` exists to make
`check()` testable, and a fake that cannot model the axis riabuild varies does not do that.

Unit tests, all against `FakeRunner`:

- renumbering: delete 3 of 5 → 4 becomes 3, 5 becomes 4, other ids unmoved
- `primary 3` moves one account and preserves the relative order of the rest
- the cap: a tenth account is refused, and the message says how many there are
- migration: legacy `claude_profile` becomes account 1; both fields present prefers the
  list; the legacy key is gone from the file after a save
- adoption: an unregistered UUID directory is adopted, oldest first
- pruning: a registered directory that vanished is dropped from the list
- status: one signed-in and one signed-out account, told apart by `CLAUDE_CONFIG_DIR`
- status: output that will not parse is `Unknown`, never `LoggedOut`
- the box: three accounts, one of each identity; a single account hides `delete` and
  `primary`; a fully signed-in list hides `Log in:`; nine accounts hide `Add an account:`
- shims: the script never execs the bare word `claude`; `"$@"` reaches it; `--settings` is
  passed; `bin/c` and `claude-4` are pruned when three accounts remain
- shell: `CLAUDE_CONFIG_DIR` is not in `environment()`
- `delete`: logout is called before the directory is removed (assert call order)
- `delete`: the only account is refused, and nothing is removed
- `delete`: no terminal and no `--yes` is a `Failure`, and nothing is removed
- `new`: a failed login removes the directory and leaves the list unchanged
- trust: two accounts, one trusted, is `Needs`; `apply` trusts both
- task: each `check()` row above, including `account 1 is not signed in`
- task: an abandoned `claude auth login` in `apply()` is an error, not a success

Three `#[ignore]`d tests, run on a machine with Claude Code installed and before every
Claude Code version bump, extending the existing `claude_config_dir_smoke`:

- `auth_status_is_scoped_to_the_config_dir` — a fresh directory reports `loggedIn: false`
  while the developer's own reports an email. This is the behaviour the whole feature
  rests on; an upstream change must surface as a test failure rather than as two accounts
  sharing one login.
- `auth_status_reports_an_email_field` — the JSON key the box reads still exists.
- `settings_flag_survives_a_subcommand` — `claude --settings <file> auth status` is
  accepted, so per-account login through the shims keeps working.

## Out of scope

No per-account nicknames — the number and the email identify an account well enough, and a
nickname is another thing to keep in sync with a list that renumbers. No per-account org
settings; org policy is org-wide by definition. No automatic account switching by
repository or by usage limit — riabuild provisions, it does not schedule.
