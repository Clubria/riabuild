# riabuild-cli

Rust binary that provisions a developer's machine and drops them into the Clubria
environment. Distributed via the Homebrew tap `clubria/tap` on macOS, and via apt and
dnf repositories on Linux — all three served from this repository.

Root conventions and the PR workflow rule are in `../CLAUDE.md`. Design is in
`../docs/superpowers/specs/2026-08-04-riabuild-design.md`.

## Commands

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
cargo run
```

## One shared `target/` across worktrees

A debug `target/` for this crate runs to roughly 1.8G, most of it a dependency graph
identical on every branch. Half a dozen worktrees each building their own copy is the
fastest way to fill a disk, so they all compile into one directory instead.

Setup is a single **untracked** file at the *repository root* — not in a worktree.
`.claude/hooks/ensure-shared-cargo-target.sh` writes it from a `SessionStart` hook, so a
fresh clone is configured before anyone builds twice. It never overwrites an existing
config, so pointing `target-dir` at another disk survives. To do it by hand:

```sh
mkdir -p .cargo
printf '[build]\ntarget-dir = "target"\n' > .cargo/config.toml
```

Deleting the file opts this machine out until the next session start.

Cargo finds `.cargo/config.toml` by walking up from the current directory to the
filesystem root, and resolves a relative `target-dir` against the directory holding
`.cargo` — not against the package or the cwd. Worktrees live under `.claude/worktrees/`,
physically inside the repository root, so they inherit that one file and every build,
main checkout and worktree alike, lands in `<repo>/target`.

**It cannot be committed.** Git copies tracked files into every worktree, cargo reads the
nearest config, and each worktree would then resolve `target-dir` against itself — handing
back the private `target/` directories this exists to remove. No relative path serves both
either, since a worktree sits three levels below the main checkout. The `.gitignore` entry
for `/.cargo/` is what keeps a well-meaning `git add` from breaking it.

Two consequences:

- **Concurrent builds serialise.** Cargo takes an exclusive lock on the build directory,
  so a second worktree building at the same time prints `Blocking waiting for file lock`
  and waits. This trades wall-clock for disk.
- **CI is unaffected.** It never sees the untracked file, so `target/` there stays at
  `riabuild-cli/target` and the `Swatinem/rust-cache` setup in `.github/workflows/ci.yml`
  needs no change.

Once it is in place the old per-worktree `riabuild-cli/target` directories are orphaned,
and deleting them is what actually reclaims the space.

## Invariants

These are not style preferences. Breaking any of them produces a class of bug that is
expensive to find on someone else's laptop.

**Every external process goes through `CommandRunner`.** No direct `std::process::Command`
outside `runner/`. This is what makes `check()` unit-testable against canned `gh`,
`git`, `node`, and `claude` output. Bypassing it means the only way to test a task is to
have a real machine in a real state, and the suite gets abandoned.

**A command riabuild runs never inherits the directory riabuild was started in.**
`RunOptions.cwd` of `None` does not mean "inherit" — for every `CommandRunner` method
but `run_interactive` it means riabuild chose no directory, and `RealRunner` runs the
child at the filesystem root. The developer's working directory is the one input riabuild
never chose, and tools read it: pnpm 11 walks up from it for a `package.json` and, on a
`packageManager` field naming another pnpm, downloads that version and hands the command
over to it, so `pnpm -v` answers for the *directory* rather than for the binary. A
`check()` built on that reports drift the `apply()` after it cannot repair — a hard error
on a machine with nothing wrong with it, on every run, until the developer happens to
stand somewhere else. `infisical` reads `.infisical.json` the same way and Claude Code
reads `.claude/`, so this is a class, not one tool.

Naming a directory is still how a command that belongs to one gets there — `infisical
export` in the checkout — and passing the path as an argument (`git -C`) is equally good.
What is never right is leaving it to chance. Do not "fix" a version probe by pointing it
at the checkout: the Clubria repo pins pnpm too, so the probe would report the pin
whatever binary was installed, and the check would go green on a broken machine.

`run_interactive` is the exception, and for the same reason it is the exception to the
async-IO rule below: it is a handoff. A developer given the environment shell somewhere
other than where they were standing would be riabuild moving them without being asked.
The rule itself is `runner::directory_for_riabuild`, split out so it is testable without
spawning anything.

**All IO is async.** riabuild runs on a current-thread tokio runtime. Filesystem work goes
through `tokio::fs`, HTTP through `reqwest`, and subprocesses through `tokio::process` —
never `std::fs` or `std::process`. A blocking call on the runtime thread stalls every
other future on it, and the symptom is a provisioner that hangs on someone else's laptop
with no output and no error to send anyone.

The exception is **stdio**. `ui.rs` writes with `println!`/`eprintln!`, and
`run_interactive` hands the terminal to a child process — that is a handoff, not IO
riabuild performs. Async stdout buys nothing for line-at-a-time terminal output.

**Except for a subdued child.** `RunOptions.subdued` runs a child under a pty riabuild
owns, so riabuild *does* perform that IO: it reads the child's output, drops every escape
sequence in it, and prints what is left as dimmed lines. That is why `runner/pty.rs` pumps
through `AsyncFd` and never a blocking read — a subdued `sudo apt-get` holds this loop for
as long as the developer takes to type a password, and a blocking read would hold the
reactor with it. The handoff remains the default and the rule everywhere `subdued` is
`None`, which is every site except apt, dnf, `gh auth login`, and `ssh-copy-id`.

Three things are synchronous because they are not IO, and are not exceptions to anything:
`paths.rs` computes paths without touching the disk, `CommandRunner::which` stats `PATH`
candidates, and tarball extraction is CPU work over an in-memory buffer — `extract_tarball`
writes through the `tar` crate, which is synchronous, so making the directory calls around
it async would be theatre.

Note that `tokio::fs` is `std::fs` on a blocking threadpool: no portable async file API
exists. "Current-thread" describes the reactor, not the process, and the binary does have
threads. Closures cannot be async, so `and_then`/`unwrap_or_else` chains around IO have to
be unrolled into `match` or `let else` rather than kept for tidiness.

**Every prompt has a default.** `Ui::ask` returns `None` when there is no terminal — in
CI, over a pipe, under `cargo test` — so a question is how riabuild offers a choice, never
how it obtains a value it cannot otherwise get. A prompt that is the only route to an
answer turns an unattended run into one that hangs with no output until something times
out. Prompts also belong in `apply()` or a subcommand, never in `check()`, which runs
under `--check`.

`ui::prompt` also has an `ask_required` / `confirm_required` pair, which returns
`Result` and fails when nobody can answer. That is not an exemption from the rule
above — it is the rule made explicit for the one case it does not cover:
`riabuild remote` with no saved server has to learn a hostname, and there is no
default that could be right. So it *refuses*, loudly and immediately, instead of
inventing an answer or blocking on a read that will never return. If you find
yourself reaching for the `_required` pair anywhere a sensible default exists, use
`ask`/`confirm` instead.

**`apply()` must be safe to run twice.** Tasks re-run whenever a dependency changes, a
version bumps, or a check fails. There is no "already done" branch to rely on.

**`apply()` is always followed by a re-run of `check()`.** If the check still fails, that
is a hard error surfaced to the developer — never a silently recorded success. Half the
value of a provisioner is telling the truth about the machine.

**`check()` is authoritative.** `version()` is only a forced-rerun escape hatch for drift
that `check()` genuinely cannot observe. If you find yourself bumping `version()` to work
around a check that does not detect a real state, fix the check.

That holds on the *first* run too, and the engine used to make an exception there — no
record in `state.json` meant `apply()` without ever asking. `state.json` is riabuild's
memory, not the machine's state, and something other than a previous run can have put a
machine in shape already: `riabuild remote` writes a server's session token into its
namespace before that server's riabuild has ever started, so `login` arrives at its first
run already signed in. Applying anyway cost the developer a second browser approval for a
token the server was holding. A first run is a reason to *ask* `check()`, never a reason
to skip it.

**No secrets in `~/.riabuild/`.** The riabuild session token goes in the Keychain via
`keychain.rs`. Infisical tokens are short-lived, brokered per use, and piped straight into
`infisical export` — never written down.

A riabuild-managed **server** is the one exception: it may hold its own session
token at `<namespace>/session.token`, mode 0600. It has no keyring, the token is
minted for that server alone, it is labelled and listed in the dashboard, and
`riabuild remote forget` revokes it. Laptops are unchanged, and the Infisical
credential is still brokered per use and never written down.

A **server's SSH password** is the second, and it is the same exception widened
to cover a keyring-less *laptop*. `riabuild remote` falls back to a password when
riabuild's key cannot sign in, and one run opens around ten SSH connections — so
the password is asked for once and kept, under `remote-password:<hash>`. The
keychain holds it wherever there is one (`security`, `secret-tool`); a machine
with neither — a container, a CI runner, a minimal distro — gets
`~/.riabuild/ssh/passwords/<hash>` at 0600 in a directory created at 0700,
because the alternative there is not "no password on disk", it is riabuild asking
again at every connection. `riabuild remote forget` deletes it beside the
session. `keychain::select_password_store` owns that choice and is the only place
that decides it. What the invariant was written to protect — the Infisical org
credential — is untouched and still brokered per use.

**`security(1)` takes the token in argv, and that is not an oversight.** Every other
secret riabuild hands a child travels on stdin, because argv is world-readable through
`ps`. macOS is the one exception: `security add-generic-password` has no stdin path for a
password. `-w` with nothing after it does not read the pipe — it calls `readpassphrase(3)`,
which opens **`/dev/tty`** and asks the human, falling back to stdin only when `/dev/tty`
cannot be opened. Every place a test can run is such a place, so the stdin spelling passed
CI, passed the macOS e2e job, and shipped; on a laptop it stopped `riabuild remote` at
`password data for new item:`, stored an empty password, and re-minted a device session on
every run thereafter. `SecurityCliKeychain::set` carries the full argument. Do not
"restore" the pipe.

The general lesson outlives the one command: a piped stdin is not proof a child cannot
prompt. Password readers open `/dev/tty` on purpose — `sudo`, `ssh` and `security` all do
— and no runner has a controlling terminal, so this whole class of bug is invisible to
every gate this repository has. Where the check cannot exist in CI, put it in the code:
`set` reads the token back and fails loudly rather than reporting a save that did not
happen.

**State is read under a lock, and written by rename.** `config.json`, `state.json` and
`remotes.json` change through `update`, never a `save` — there is no `save`, and adding one
back is how the bug returns. `update` takes `~/.riabuild/.state.lock`, reads what is on
disk *now*, applies the closure, and lands the result by `rename` from a temporary beside
it. The read is inside the lock because riabuild can be running in two terminal windows:
loading at process start and writing back at save time means the later writer wins with a
snapshot from whenever it began, which drops a Claude account from the registry while its
directory stays on disk — and since position *is* the account number, adopting that orphan
later changes which account `claude-2` opens. `Ctx.config` and `Ctx.state` are read-only
snapshots for the run; `ctx.update_config` and `ctx.update_state` refresh them from what
actually landed. A test that seeds them in memory alone is seeding a fiction, and will see
it discarded on the first write.

`engine::run_all` additionally holds `~/.riabuild/.provision.lock`, so two runs do not
install one toolchain twice. It is taken *after* `update::upgrade_and_reexec`, because a
`flock` survives `exec`, and dropped *before* the shell handoff, which awaits the
developer's terminal for hours. `--check` takes neither, and both are namespaced per
developer rather than per machine — a box-wide lock would let one developer on a server
block another under the Unix account they share.

Both are `std::fs::File::try_lock`, then a reported wait, then a blocking `lock()` on the
blocking pool — cargo's sequence, including its rule that a filesystem which cannot lock
is treated as success rather than a reason to refuse to provision. Design:
`../docs/superpowers/specs/2026-08-12-concurrent-runs-design.md`.

**Paths and keychain stay behind traits.** macOS and Linux are both supported, and
`paths.rs`, `keychain/`, `tools.rs`, `download/` and `update.rs` are the only files
that may know which one they are running on. A `cfg!(target_os)` or a
`std::env::consts::OS` anywhere else is a bug — it puts a platform decision somewhere no
test on the other platform can reach it.

Where a platform answer is a value rather than a code path, take the OS as a *parameter*
and keep a thin wrapper that supplies the real one. `paths::default_project_dir_on` is the
pattern: `cfg!` would compile every branch but one out of the test binary, so only the
runner's own answer could ever be asserted.

**riabuild owns every tool it depends on.** Node, pnpm, Claude Code, `gh`, and
`infisical` are downloaded, verified against a published digest, and kept under
`~/.riabuild/`. Nothing on the developer's `PATH` is trusted, and no task shells out to a
package manager to install a dependency. Run them through `ctx.gh()` and `ctx.infisical()`
rather than by name: during provisioning `~/.riabuild/bin` is not on `PATH`, so the bare
name finds a binary no `check()` verified, or nothing at all.

Pinned versions live in `tools.rs` as constants, never a `releases/latest` lookup —
what riabuild puts on a laptop should be versioned, auditable, and shipped in a signed
release. Bumping one means bumping the task's `version()` beside it.

**The version comes from the git tag, never from `Cargo.toml`.** riabuild is versioned by
release date (`2026.08.04`), which semver cannot express, so `Cargo.toml` holds a
permanent `0.0.0` placeholder and `cli.rs` reads `RIABUILD_VERSION` injected by the
release workflow. Do not bump the crate version and do not reintroduce
`CARGO_PKG_VERSION` — a binary reporting a version other than the release it shipped in
makes every launch attempt an upgrade that cannot change anything. Local builds report
`9999.0.0-dev`, above every real date, so working on riabuild never makes riabuild replace
the binary being worked on. See `../docs/releasing.md`.

**Self-update asks what owns the binary, never what is installed.** `update.rs` runs
`dpkg -S` and then `rpm -qf` against the running executable. A Fedora machine can have
`apt` on it, and a riabuild built with `cargo` is owned by nothing — `sudo apt-get install
riabuild` there installs a *second* riabuild elsewhere and leaves this one in place, so
every upgrade reports success and nothing changes. That case prints the command and never
sudoes.

## Adding or changing a setup task

Read `.claude/skills/writing-setup-tasks/SKILL.md` first. It covers the `Task` trait,
when to bump `version()`, how to write a check that actually detects drift, and the
dependency edges you must declare.

## Layout

```
src/
  main.rs      entry point, dispatch   cli.rs       clap definitions
  provision.rs the default flow        internal.rs  `riabuild internal …` handlers
  config.rs    ~/.riabuild + state     paths.rs     path resolution (trait)
  keychain/    secret storage: trait,  runner/      CommandRunner — all subprocesses.
               the two platform CLIs,               `subdue.rs` is the line filter a
               the server's file store              subdued child's output goes through;
                                                    `pty.rs` is the terminal it gets
                                                    instead of riabuild's own
  update.rs    version check, re-exec  ui.rs        output and prompts
  theme.rs     the Clubria palette,    art.rs       the riabuild mark: two
               by role, and the                     renderings and the banner
               depth ladder under it                laid out around them
  scope.rs     laptop vs. server, from gh_session/  where the GitHub config dir
               RIABUILD_REMOTE, and the             goes, how it is created safely
               namespace it implies: member         against a co-tenant, and how
               id, server session token file        long it lives
  move_project.rs  `move-project`      fs_move.rs   rename, or copy across filesystems
  reset.rs     removes ~/.riabuild
  tools.rs     the gh and infisical releases riabuild owns
  version.rs   parsing and comparison  testing.rs   test helpers
  api/         riabuild-web client     tasks/       trait, registry, DAG runner, one file per task
  download/    where a release lives, what its asset is called, and the digest that
               says the bytes are the ones upstream published
  archive/     unpacking what download fetched: tar and zip, one member or a whole
               tree, and `staging` for landing that tree atomically
  shell/       zsh, bash, fish         shims/       ~/.riabuild/bin generation
  channel/     the laptop channel: clipboard and browser over the SSH reverse-forward.
               `socket.rs` decides where that socket lives and refuses one that is not
               ours; `supervisor/` keeps the forward up and proves it carries traffic
  accounts/    the Claude Code accounts: registry, status, box, `riabuild claude`
  remote/      remote mode: `riabuild remote` / `list` / `forget` — identity, host-key
               trust, authorising a key, installing the server's own binary, minting its
               session, seeding a GitHub sign-in, and the mosh/ssh shell handoff.
               `askpass.rs` answers the password prompt when the key cannot sign
               in: the SSH_ASKPASS shim, the account the password is saved under,
               and the environment every ssh in remote mode carries. `pick.rs` is
               the prompt a bare `riabuild remote` puts — one of the saved
               servers, or a new one — and `render.rs` the box it and `list` show
```

`download/` decides where bytes come from and whether they are the right bytes;
`archive/` only ever sees a buffer that already matched a digest. Keep that split — it
is what makes "verified before anything is written" a property of the code rather than a
convention.

**The clipboard channel's socket is namespaced, and never unlinked.** It lives at
`<namespace>/channel.sock`, not in the runtime directory `socket_path()` would otherwise
pick. Developers on a server share one Unix account, so they share one uid and one
`$XDG_RUNTIME_DIR` — leaving the server to resolve its own path would hand every
developer on the box the same socket, and one developer's `xclip` would read another's
laptop. Its parent is created **at** mode 0700 rather than created and then chmod'd, so
it never exists at the umask even briefly, and a path that is a symlink or owned by
another uid is refused rather than removed: unlinking is how you take over someone
else's channel, not how you recover from a stale one.

The health probe runs **on the server**, not against the laptop's own socket. The forward
runs server-to-laptop, so only a probe originating there can see it wedged — and a wedged
forward is precisely what SSH's own keepalives cannot detect, because they run below it.
A local socket check would test the agent's liveness, report a dead tunnel as healthy, and
look like a tidy optimisation while deleting the guarantee.

Inside `archive/`, `staging.rs` owns *how* a tree lands: unpack into a sibling
directory and `rename` it into place, never `remove_dir_all` the target first.
`tools_root()` is shared by every developer with an account on a server, so the
tree being replaced may be one a colleague's `pnpm dev` is running out of. A
delete-then-unpack is correct on a single-user laptop and silently destructive
anywhere else — and it compiles and passes every test either way, because no
test has a co-tenant.

One task per file. Roughly 300 lines of **production** code is the point at which
a file is doing too much — `#[cfg(test)]` modules do not count towards it. The number
is about how much behaviour one file owns, and a test module is not behaviour: a
small implementation under a long test module is the shape this repo wants, and
counting the tests would make writing more of them look like a problem.

## Claude Code accounts

A developer has an ordered list of up to nine Claude Code accounts, each a
`~/.riabuild/claude/<uuid>/` config directory with its own sign-in, and each reached by its
own generated launcher: `claude` runs the primary, `claude-1` … `claude-N` run a particular
one. The launchers are the only thing that names a config directory — `CLAUDE_CONFIG_DIR` is
deliberately **not** exported into the environment shell, so a `claude` started outside a
launcher cannot land in an account by accident, and one exported value cannot quietly make
all nine share a directory. `riabuild claude list|new|delete|primary` manages the list,
every environment shell opens with the account box, and the org's Claude settings and the
checkout's trust apply to every account, never just the first.

Design: `../docs/superpowers/specs/2026-08-06-claude-accounts-design.md`.

## Colour

Every colour riabuild prints comes from `theme.rs`, chosen by **role** — `Ok`, `Busy`,
`Danger`, `Brand`, `Muted` — never by writing an escape code at the call site. A role
renders itself at each rung of a depth ladder (24-bit → 256 → the original sixteen →
nothing), so a terminal that cannot do truecolor still gets something deliberate, and
`NO_COLOR` or a non-tty destination gets no escapes at all.

The palette is Clubria's own, read from clubria.com: `#f74f25` is the logo mark's fill,
with `--pink`, `--orange` and `--green` beside it. `Muted` and `Strong` stay *attributes*
(dim, bold) rather than becoming a fixed grey — a hardcoded grey is invisible on one
terminal theme and muddy on another.

**Two roles on one line are siblings, never nested.** `Theme::paint` closes with
`\x1b[0m`, which resets every attribute rather than the one it opened, so a `Strong`
value formatted *into* a `Muted` line ends the dim at the value and undims everything
after it. `ui::note_value` is the shape that gets this right — muted prose, an
emphasised tail — and it exists because a device code printed dim is the least legible
thing on a screen whose only purpose is that code. Reach for `Strong` there rather than
a hue: `Brand` and `Danger` share `1;31` on a sixteen-colour terminal, and a
brand-coloured code reads as an error on exactly the old SSH sessions where the
device-code flow is the entire interface.

Text printed by a generated rcfile — the shell banner, the accounts box — takes a `Theme`
as a parameter rather than reading one, because it is rendered on this side of the
boundary and printed on the other. Pass `ctx.ui.theme()`, not `ctx.ui.colour()`: the
latter answers only whether colour is on, which would quietly pin that text to the
sixteen-colour rung.

**Child output has a role too.** A subdued child's lines are `Muted`, printed at
`ui::note`'s indent, and everything the child drew *with* — colour, vertical cursor
motion, the alternate screen, an OSC window title — is dropped before it reaches the
terminal. riabuild does not trust a third-party program to keep a tidy terminal, so under
`RunOptions.subdued` it does not have to. `subdued` takes a `Theme` for the same reason
rcfile text does, and for the same reason it is `ctx.ui.theme()` that gets passed.

Subduing is for the commands riabuild runs *at* the developer — apt, dnf,
`gh auth login`, `ssh-copy-id`. Not the environment shell, not `ssh`/`mosh`, not
`claude`: that is the developer's workspace, not riabuild's output. Not the clipboard
shim either, where riabuild is impersonating `xclip` and its stdout is a payload rather
than a page. Design:
`../docs/superpowers/specs/2026-08-12-subdued-child-output-design.md`.

## Shell integration

`bash --rcfile` **replaces** the user's `.bashrc`; zsh has no `--rcfile` and needs
`ZDOTDIR`. Generated rcfiles must source the user's real config **first**, then apply
riabuild's environment. Getting this wrong silently destroys a developer's prompt,
aliases, and history config, which reads as *riabuild broke my shell*.
