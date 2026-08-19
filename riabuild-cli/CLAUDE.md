# riabuild-cli

Rust binary that provisions a developer's machine and drops them into the Clubria
environment. Distributed via the Homebrew tap `clubria/tap` on macOS, and via apt and
dnf repositories on Linux — all three served from this repository.

Root conventions and the PR workflow rule are in `../CLAUDE.md`. Design is in
`../docs/superpowers/specs/2026-08-04-riabuild-design.md`.

## Commands

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p riabuild-cli        # `-p` because the root is a virtual manifest
cargo test -p riabuild-remote    # one crate, one test binary, no relink of the rest
```

## One shared dependency graph, one `target/` per worktree

A debug build of this workspace runs to several gigabytes, and it splits in two. The
dependency graph is identical on every branch and is worth compiling once. The finished
binaries are not: a debug `riabuild` alone is 170M, and it is the one file that must mean
*this* worktree's code. Cargo separates the two, so we let it.

```
<repo>/shared-build/           one copy, every checkout and worktree
  debug/{deps,build,incremental,.fingerprint}
  debug/.cargo-build-lock      what makes concurrent builds serialise

<repo>/riabuild-cli/target/                         the main checkout's own
<repo>/.claude/worktrees/<wt>/riabuild-cli/target/  one per worktree
  debug/riabuild               the binary that worktree built
```

Setup is a single **untracked** file at the *repository root* — not in a worktree.
`.claude/hooks/ensure-shared-cargo-build-dir.sh` writes it from a `SessionStart` hook, so
a fresh clone is configured before anyone builds twice. It never overwrites a config
someone customised, so pointing the build at another disk survives. To do it by hand:

```sh
mkdir -p .cargo
printf '[build]\nbuild-dir = "shared-build"\n' > .cargo/config.toml
```

Deleting the file opts this machine out until the next session start.

`target-dir` is deliberately **absent**. Left unset it defaults to
`<workspace-root>/target`, which is per-worktree — and that is the whole mechanism.
Setting it, as this repo did until 2026-08-14, is what made every worktree write one
shared `target/debug/riabuild`, so the binary you ran was whichever build finished last.
That is also the layout CI already assumes, which is why `e2e/run.sh` and the
`RIABUILD_BIN` values in `.github/workflows/ci.yml` name `riabuild-cli/target/...` and
now find it locally too.

Cargo finds `.cargo/config.toml` by walking up from the current directory to the
filesystem root, and resolves a relative `build-dir` against the directory holding
`.cargo` — not against the package or the cwd. Worktrees live under `.claude/worktrees/`,
physically inside the repository root, so they inherit that one file and every build,
main checkout and worktree alike, compiles into `<repo>/shared-build`.

**It cannot be committed.** Git copies tracked files into every worktree, cargo reads the
nearest config, and each worktree would then resolve `build-dir` against itself — handing
back the private dependency graphs this exists to remove. No relative path serves both
either, since a worktree sits three levels below the main checkout. The `.gitignore` entry
for `/.cargo/` is what keeps a well-meaning `git add` from breaking it.

Three consequences:

- **Concurrent builds still serialise.** The lock lives in the shared directory, so a
  second worktree building at the same time prints `Blocking waiting for file lock` and
  waits. This trades wall-clock for disk, as it always did.
- **CI is unaffected.** It never sees the untracked file, so with no `build-dir` set both
  halves land under `riabuild-cli/target` and the `Swatinem/rust-cache` setup in
  `.github/workflows/ci.yml` needs no change.
- **A worktree can still be handed another one's build.** See below. This is known and
  accepted, not a bug waiting to be reported.

### The part this does not fix

Cargo identifies a workspace package by name, version, features and flags — **never by
path** — and decides freshness by mtime against a dep-info file written with *relative*
source paths. Both are deliberate: they are what lets `Swatinem/rust-cache` restore a
cache into a differently-named CI checkout. The cost is that two worktrees of riabuild at
the same version are, to cargo, the same package. They share one fingerprint slot in
`shared-build`, and the one whose sources have older mtimes is declared fresh against the
other's output — `cargo build` prints `Finished` in 0.01s and uplifts the other worktree's
binary into this worktree's `target/`.

Symptoms: a change you just made appears not to take effect, or a fix "comes back". It
bites on the *return* visit to an older worktree, because `git worktree add` stamps
mtime=now and a new worktree therefore rebuilds honestly.

Neither cargo escape hatch is available on stable — `--artifact-dir` and
`build.checksum-freshness` are both nightly-only as of 1.97.1. To force the rebuild, drop
the fingerprints for riabuild's own crates; every dependency survives untouched:

```sh
rm -rf shared-build/debug/.fingerprint/riabuild*
```

Before debugging a binary that seems to have travelled through time, prove which build
ran: `riabuild --version` and the file's mtime.

### Migrating an existing machine

The hook upgrades the old config in place on the next session start and prints what to do
with the directory it orphans. `<repo>/target` held both halves; renaming it keeps the
warm cache and skips recompiling every dependency once:

```sh
mv <repo>/target <repo>/shared-build   # keep the cache
rm -rf <repo>/target                   # or just reclaim the space
```

The stray uplifted binaries left at `shared-build/debug/riabuild` and
`shared-build/debug/lib*.rlib` are inert; cargo reads neither.

## Invariants

These are not style preferences. Breaking any of them produces a class of bug that is
expensive to find on someone else's laptop.

**Every external process goes through `CommandRunner`.** No direct `std::process::Command`
outside `riabuild-runner` — which the crate graph now enforces, since it is the only
crate that names `tokio/process` at all. This is what makes `check()` unit-testable against canned `gh`,
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
`None`, which is every site except apt, dnf, and `gh auth login`.

**A subdued child may never ask the terminal a question.** Dropping every escape sequence
drops the ones that are *questions*, and under a pty riabuild owns riabuild is the only
thing that could answer them — it answers none. A child that asks waits for ever, and it
waits while eating the developer's keystrokes, because the read it is blocked in is looking
for a reply rather than for input. `gh auth login` used to open with a `survey` confirm
(*"Authenticate Git with your GitHub credentials? (Y/n)"*, asked **before** it
authenticates), and `survey` sizes the terminal with `ESC[999;999f` then `ESC[6n`;
`riabuild remote` sat on that line ignoring every `y`, with no device code above it to hint
why. `github_cli`'s `own_git_credentials` removes the question rather than answering it.
So the test for a new subdued site is not "is its output untidy" but "does it ask" — plain
text and a wait for a person is fine, a full-screen prompt library is not.

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

A **headless Linux machine's own session token** is the second, and it is the
first exception widened from "a server riabuild manages" to "any machine with no
keyring". `secret-tool` being installed is not the same as a Secret Service
answering: `libsecret-tools` arrives as a transitive dependency all over the
place, and a box with no D-Bus session bus has the binary and nothing listening.
Such a machine used to have *no* branch at all — it was handed the keyring
store, ran the whole device-code flow, and failed on the write, discarding a
session the developer had just approved in a browser and printing `secret-tool:
Cannot autolaunch D-Bus without X11 $DISPLAY` under "it is a bug in riabuild".
It now gets `~/.riabuild/session.token` at 0600 in a directory at 0700, the same
`FileKeychain` a managed server has always used.

The design spec for Linux support ruled this out, on the grounds that
"`RIABUILD_TOKEN` already covers the headless case". It does not and never did:
`RIABUILD_TOKEN` is a CI and e2e hook, the dashboard has no screen that shows a
developer a token to copy, and the string never appears in riabuild-web at all.
That bullet is corrected in the spec rather than left standing.

The **cache of a server's session on a keyring-less laptop** is the same
widening once more, at `~/.riabuild/remote-sessions/<hash>`. `riabuild remote`
reads that cache so a second run finds the server's token without re-minting;
without a fallback it errored, and `riabuild remote` was unusable from any
laptop with no libsecret — `e2e/remote/run.sh` carried it as "known gap (a)".
Storing it is also the *conservative* option: the alternative is not "no token
on disk", it is a fresh 90-day session minted on every run and recorded nowhere
this laptop can revoke it. `riabuild remote forget` deletes it with the rest.

`keychain::keyring_answers` owns the "is there a keyring here?" question and is
the only place that decides it — `runner.which("secret-tool")` is not an answer
to it, and reintroducing that test is how this bug comes back. All three call
sites (`for_platform`, `for_password`, `for_account`) ask it; a fourth that
asks `which` instead will look correct, pass CI, and fail on a server.
`describe()` must name where the token really went: `provision.rs` prints it,
and it is the only line telling the developer their token is in a file rather
than a keyring.

A **server's SSH password** is the third, and it is the same exception widened
to cover a keyring-less *laptop*. `riabuild remote` falls back to a password when
riabuild's key cannot sign in, and one run opens around ten SSH connections — so
the password is asked for once and kept, under `remote-password:<hash>`. The
keychain holds it wherever there is one (`security`, `secret-tool`); a machine
with neither — a container, a CI runner, a minimal distro — gets
`~/.riabuild/ssh/passwords/<hash>` at 0600 in a directory created at 0700,
because the alternative there is not "no password on disk", it is riabuild asking
again at every connection. `riabuild remote forget` deletes it beside the
session. `keychain::select_password_store` owns that choice and is the only place
that decides it — and it asks `keyring_answers`, not `which`, for the same
reason the session token does. What the invariant was written to protect — the Infisical org
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

**A shared server's address is never read off the disk.** `remotes.json` holds one for
each of the team's servers, but only as a copy — the record is `Origin::Stale` until this
run's fetch of `/api/v1/remotes/shared` describes it, and nothing that leads to a
connection looks at a `Stale` record. `Record::fresh` is `#[serde(skip)]` precisely so
that `false` is what a record read off the disk gets. Do not "fix" that by persisting it:
the default being the safe one is the whole mechanism, and without it a lead's edit or a
server they removed would go on being connected to forever.

What the copy is *for* is the other half: an address is an identity — `Remote::hash` is
taken over `user@host:port` — so when a lead edits one, the old address is what
`forget::retire_identity` needs to revoke the session and clear the key from the machine
being left. A record also carries the `sharedId` it is keyed by, which is the riabuild-web
row id and the only field that survives both a rename and a re-address. The display name
(`shared-<name>`) is what `find`, `persist_one` and `forget_one` all match on, because the
bare name does not identify a row: a shared `gpu` and a `gpu` the developer added share
it. Design: `../docs/superpowers/specs/2026-08-12-shared-servers-design.md`.

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
`riabuild-paths`, `riabuild-keychain`, `riabuild-fetch`'s `tools` and `download`, and
the binary's `update.rs` are the only places
that may know which one they are running on. A `cfg!(target_os)` or a
`std::env::consts::OS` anywhere else is a bug — it puts a platform decision somewhere no
test on the other platform can reach it.

Where a platform answer is a value rather than a code path, take the OS as a *parameter*
and keep a thin wrapper that supplies the real one. `paths::default_project_dir_on` is the
pattern: `cfg!` would compile every branch but one out of the test binary, so only the
runner's own answer could ever be asserted.

**The parameter has to reach the public function, not just the decision underneath it.**
`keychain::select` took `is_macos` from the day it was written, and the three wrappers
around it — `for_platform`, `for_password`, `for_account` — went on asking `cfg!`
themselves, so every test that went *through* a wrapper silently asserted "…and the host
is Linux". Six did. They passed the pull-request gate for the whole life of the
keyring-less fallback and then failed together on the release workflow's macOS job —
which runs *after* the tag is pushed, so the first two releases that carried them had no
binaries at all. Half-applying this pattern is worse than not applying it, because the
extracted function looks like the coverage already exists.

**The gate has a Mac, and that is not the same as running your test on one.** `e2e.yml`'s
"riabuild on macOS" job runs on `pull_request`, so a macOS runner was never what was
missing — it runs the end-to-end suite, and a green tick on it has never meant a single
unit test ran on a Mac. Do not read one as unit-test coverage of a platform branch. The
parameter is what makes that branch assertable, on every host and on every run.

`ci.yml`'s **"riabuild-cli on macOS"** job is what closes the rest of the gap: the same
`cargo test --workspace --all-targets`, on `macos-latest`, on every pull request. It was
added after the second time this bit — `pump::tests` had a case that *hung* on macOS, and
because `cargo test` on a Mac then ran only in `release.yml`, which triggers on `v*`, the
first thing to notice was a release whose tag was already public and whose macOS job sat
there for twenty-five minutes building nothing. A test that only fails on one platform has
to be able to fail on a pull request. That job and the `timeout-minutes` on every
compiling job in `release.yml` are two halves of one rule: **a hang must present as a red
job, never as a slow one.** It does not repeat fmt, clippy or the packaging checks, which
answer the same on both hosts.

Each wrapper now takes `is_macos`, and `each_wrapper_passes_the_platform_it_is_actually_running_on`
pins all three to the host's real answer. That test is not optional: a parameter without
one *moves* the untested branch into the wrapper rather than removing it, and a wrapper
hardcoding `false` would send every Mac to `secret-tool` while the suite stayed green on
both hosts.

**riabuild owns every tool it depends on.** Node, pnpm, Claude Code, the Codex CLI, `gh`,
and `infisical` are downloaded, verified against a published digest, and kept under
`~/.riabuild/`. Nothing on the developer's `PATH` is trusted, and no task shells out to a
package manager to install a dependency. Run them through `ctx.gh()` and `ctx.infisical()`
rather than by name: during provisioning `~/.riabuild/bin` is not on `PATH`, so the bare
name finds a binary no `check()` verified, or nothing at all.

Pinned versions live in `riabuild-fetch`'s `tools` as constants, never a `releases/latest` lookup —
what riabuild puts on a laptop should be versioned, auditable, and shipped in a signed
release. Bumping one means bumping the task's `version()` beside it.

**The version comes from the git tag, never from `Cargo.toml`.** riabuild is versioned by
release date (`2026.08.04`), which semver cannot express, so `crates/cli/Cargo.toml`
holds a permanent `0.0.0` placeholder — the workspace root has no `[package]` and no
version at all — and `riabuild-version` reads `RIABUILD_VERSION` injected by the
release workflow. Do not bump the crate version and do not reintroduce
`CARGO_PKG_VERSION` — a binary reporting a version other than the release it shipped in
makes every launch attempt an upgrade that cannot change anything. Local builds report
`9999.0.0-dev`, above every real date, so working on riabuild never makes riabuild replace
the binary being worked on. See `../docs/releasing.md`.

**A server is never given a riabuild older than the laptop provisioning it.**
`remote::install::version_for_server` picks the newer of this laptop's own version and the
org's `latestCliVersion`, and a local build — which names no release anyone could download
— takes the org's answer. The two are halves of one protocol: the laptop runs `internal
gh-sweep` and `internal seed-github` on the server, sets its `RIABUILD_ROOT`, and reads its
exit code, and only the matched pair is ever tested. Reading `latestCliVersion` alone put
an *older* riabuild on the server every time a laptop was ahead of the org pin — including
for minutes after every release, since the workflow publishes the Homebrew formula well
before the job that moves that field. The symptom is not a version error: it is whichever
bugs the older build still had, reappearing on a laptop that carries the fixes, with
nothing in the terminal naming a version. That is why `connect` says out loud when the
server will run something other than what the laptop is running.

**Self-update asks what owns the binary, never what is installed.** `update.rs` runs
`dpkg -S` and then `rpm -qf` against the running executable. A Fedora machine can have
`apt` on it, and a riabuild built with `cargo` is owned by nothing — `sudo apt-get install
riabuild` there installs a *second* riabuild elsewhere and leaves this one in place, so
every upgrade reports success and nothing changes. That case prints the command and never
sudoes.

**riabuild updates itself on every command whose stdout is a terminal a human is
reading.** `main::keep_current` runs it once, at the top of `run_inner`, for every
invocation; it lived in `provision` alone until a developer whose day is `riabuild
remote` and `riabuild claude` turned out never to run the one command that updates
riabuild. That is not cosmetic drift — `install::version_for_server` then hands each
server a *newer* riabuild than the laptop driving it, which is exactly the unmatched
pair the invariant above forbids.

`update::applies_to` holds the four exceptions and matches `Command` exhaustively, like
`opens_shell`, so a new subcommand is a compile error rather than a silent `false`.
Each is excepted because its stdout is a **payload**, or because it must run on a
machine riabuild cannot read: `internal …` (`ssh` reads `askpass`'s stdout *as the
password*, from inside an authentication attempt), `channel …` (the clipboard and
browser shims, on every Ctrl+V), `env` (`export` lines a shell evaluates, and `Ui::info`
writes to stdout), and `reset` (dispatched before the tree is read at all). This is also
the answer to "why not update before argv is parsed": telling those four apart from
`riabuild status` is precisely what parsing argv is for.

Two things hold the rest of it up. `update::action_for` owns both guards — a managed
server never replaces its own binary, and a laptop with no session has no floor to be
below — so neither is restated anywhere. And the connect it depends on is **soft**: a
laptop that cannot reach riabuild-web carries on, because `riabuild claude` is
documented to work with no session, no network, and a machine nothing has provisioned.
The flows that genuinely need the API still call `connect` themselves and still fail
loudly; `Ctx::connect` is idempotent within a run so that costs nothing.

## Adding or changing a setup task

Read `.claude/skills/writing-setup-tasks/SKILL.md` first. It covers the `Task` trait,
when to bump `version()`, how to write a check that actually detects drift, and the
dependency edges you must declare.

## Layout

A cargo workspace. `Cargo.toml` at the root is virtual — it holds the release profile,
the lint policy, and one statement of every third-party dependency, and no code. The
crates form a straight line, each depending only on those above it:

| Crate | What | Depends on |
|---|---|---|
| `theme` | the Clubria palette, by role, and the depth ladder under it | — |
| `version` | riabuild's own `VERSION`, and version parsing and comparison | — |
| `fetch` | `download` (where bytes come from, and whether they match a published digest), `archive` (unpacking what download fetched, and `staging` for landing a tree atomically), `tools` (the gh and infisical releases riabuild owns) | — |
| `ui` | output, prompts, and the `Failure` every error becomes; `art` is the riabuild mark and the banner | theme, version |
| `runner` | `CommandRunner` — all subprocesses. `subdue` is the line filter a subdued child's output goes through; `pty` is the terminal it gets instead of riabuild's own | theme |
| `paths` | path resolution (trait), `config` (`~/.riabuild` and state), `filelock` (the lock both are read and written under) | ui |
| `keychain` | secret storage: the trait, the two platform CLIs, the file store for machines with no keyring, and `keyring_answers` — whether a Secret Service actually replies | runner, ui |
| `api` | the riabuild-web client: sessions, org configuration, brokered secrets | runner, ui |
| `gh-session` | where the GitHub config dir goes, how it is created safely against a co-tenant, and how long it lives | paths, runner, ui |
| `channel` | the laptop channel: clipboard and browser over an SSH exec session. `mux` frames many shim connections onto one pipe, `pump` is the server end that binds the socket and relays, `agent::pipe` is the laptop end; `socket` decides where that socket lives and refuses one that is not ours; `supervisor` keeps the connection up | gh-session, paths, runner, ui |
| `tasks` | the `Task` trait, the registry, the DAG runner, one file per task; `accounts` (the Claude Code accounts), `shell` (zsh, bash, fish), `shims` (`~/.riabuild/bin` generation), `scope` (laptop vs. server) | all of the above |
| `remote` | remote mode: identity, host-key trust, authorising a key, installing the server's own binary, minting its session, seeding a GitHub sign-in, and the mosh/ssh handoff. `askpass` answers the password prompt when the key cannot sign in; `pick` is the prompt a bare `riabuild remote` puts, and `render` the box it and `list` show; `shared` folds the team's servers in from riabuild-web on every run | all of the above |
| `cli` | the binary. `main` (parse argv, assemble `Ctx`, dispatch), `dispatch` (argv → library calls), `provision` (the default flow), `internal`, `reset`, `move_project`, `fs_move`, `update` | all of the above |

**The graph is the point, not the file count.** `riabuild-runner` cannot name a `Task`;
`riabuild-fetch` cannot reach the API client, so a string the server sent can never
become a URL riabuild downloads from; `riabuild-tasks` cannot reach `riabuild-remote`,
so a task cannot grow a remote-mode branch. Those were prose before, and are now
things that fail to compile.

Only the binary sees a clap type. A library that matches on a command enum has to be
compiled with the parser, and one compiled with the parser can read any flag it likes
rather than the ones its caller chose to pass — which is what `remote` was doing,
reaching back into the global `Cli` from four directories down. `dispatch.rs` holds
every argv → library mapping, and library crates take named requests.

What a crate boundary does **not** enforce: "no task shells out to Homebrew, apt or
dnf" and "secrets are brokered, never stored" both live inside a single crate. Those
still need tests.

Design: `../docs/superpowers/specs/2026-08-12-cargo-workspace-design.md`.

`download` decides where bytes come from and whether they are the right bytes;
`archive` only ever sees a buffer that already matched a digest. They are siblings
inside `riabuild-fetch`, and keeping that split is what makes "verified before anything
is written" a property of the code rather than a convention.

**The clipboard channel's socket is namespaced, and never unlinked.** It lives at
`<namespace>/channel.sock`, not in the runtime directory `socket_path()` would otherwise
pick. Developers on a server share one Unix account, so they share one uid and one
`$XDG_RUNTIME_DIR` — leaving the server to resolve its own path would hand every
developer on the box the same socket, and one developer's `xclip` would read another's
laptop. Its parent is created **at** mode 0700 rather than created and then chmod'd, so
it never exists at the umask even briefly, and a path that is a symlink or owned by
another uid is refused rather than removed: unlinking is how you take over someone
else's channel, not how you recover from a stale one.

**A generated shim names the riabuild binary in full, never `riabuild`.** riabuild is the
one tool riabuild does not put on `PATH`: it lives at
`<tools>/riabuild/<version>/riabuild`, and `shell::riabuild_path_dirs` leads `PATH` with
`bin/` and Node's `bin/` and nothing else. Every clipboard and browser shim used to
`exec riabuild …` anyway, which meant each one resolved to whatever *else* was called
riabuild on that machine. On a server with no system copy that is nothing, and
`$BROWSER` failed with `xdg-open: 8: exec: riabuild: not found` — the error a developer
cannot act on, because the file it names is right there and executable. On a server with
an apt or Homebrew copy it is worse, because it works: a *different version* answers, on
the machine the "never older than the laptop provisioning it" invariant above exists to
keep matched, with nothing in the terminal naming a version.

`shims::running_binary` is the only source of that path — `/proc/self/exe`, resolved once
by `provision::write_launchers_with` before the first shim is written, so a run that
cannot answer fails rather than writing three good shims and a broken one. It survives
the developer's `PATH`, the `claude` launcher's `PATH` strip, and a `$BROWSER` spawned by
a process that sanitised its environment; and because `upgrade_and_reexec` has already
replaced this process by the time provisioning reaches here, it names the version that
will actually run. `no_shim_looks_riabuild_up_on_the_path` covers every generated shim at
once, so one added later is covered on the day it is written.

The same condition now writes `bin/riabuild`, which is what makes `riabuild` a working
command *on a server* — it was `command not found` there, or quietly the box's own copy.

**Clearing `SSH_CONNECTION` is half of reaching the clipboard shims.** Claude Code's read
path is gated on the SSH variables alone, but its *write* path runs a Linux probe that
asks for `WAYLAND_DISPLAY` before it looks for `wl-copy` and `DISPLAY` before `xclip`. A
headless server has neither, so the probe records "no clipboard tool here" without ever
consulting `PATH`, and every copy leaves as the OSC 52 escape `setClipboard` returns
unconditionally — pastes working and copies not, which is not a symptom anyone reads as
one bug. The `claude` launcher claims `WAYLAND_DISPLAY` where riabuild's own `wl-copy` is
what the probe will find and the machine has no display of its own. Verified against
2.1.232; both this and the `unset` above are undocumented, so re-read them when the
pinned Claude Code version moves. Design:
`../docs/superpowers/specs/2026-08-07-clipboard-channel-design.md`.

**The transport is `ssh -T <host> riabuild channel pump`, and it must stay that way.** The
channel asks an SSH server to run a command and for nothing else — no `-R`, no
`AllowStreamLocalForwarding`, no `StreamLocalBindUnlink`, no line in anyone's
`sshd_config`. Command execution is a floor remote mode already stands on (setup,
`session::ensure`, installing the server's binary, the shell itself), so a channel built on
it needs no permission the session did not already have. Reaching for a forward again — for
a second socket, for a "simpler" path — reintroduces a dependency that hardened servers
refuse outright and that some SSH implementations have never implemented.

It also put the socket's lifecycle in the wrong hands. Under `-R` **sshd** called `bind()`,
so whether a stale socket could be replaced was `sshd_config`'s `StreamLocalBindUnlink`
(default `no`); the `-o StreamLocalBindUnlink=yes` riabuild passed was a *client* option
governing only sockets `ssh` itself creates, i.e. `-L`. It did nothing, and one
`channel.sock` left by a killed session disabled paste on that server permanently, with no
riabuild flag able to clear it. The pump owns the bind now, so clearing a dead socket is an
ordinary `unlink` by its owner — and a socket that still *answers* is refused rather than
taken, because taking one silently cuts a colleague's session.

**The channel's `ssh` is reached by remote mode's own rules, never by an argv the
supervisor composes.** `supervisor::Tunnel` takes `options: Vec<String>` — the list
`identity::ssh_options` builds, the same one behind the setup run, the mosh bootstrap and
the developer's shell — and `ssh_args` adds only what is its own: `-T`, the keepalives,
`BatchMode=yes`. It used to take a `port` and an `identity` and build `-p`/`-i` itself,
which looked complete and quietly reached servers by different rules than the connection
beside it. Two omissions were fatal and neither said so:

- riabuild records a host key in **its own** `known_hosts`, never `~/.ssh/known_hosts`, so
  without `UserKnownHostsFile` the channel's `ssh` read the developer's file, did not find
  the host, and — correctly, under `BatchMode=yes` — exited `Host key verification failed`.
  A box the developer had once `ssh`'d to by hand worked; one only riabuild had ever
  reached never did, which is as confusing a pair of servers as it sounds.
- a **carried issued identity** (`IdentityAgent`) never reached it, so the servers that
  feature exists for — the ones riabuild's own key cannot sign in to — could not carry a
  channel at all, however well the rest of the session worked.

It also opted out of `-F /dev/null`, leaving the one connection nobody watches redirectable
by a `Host` block in `~/.ssh/config`. The rule is the one `Tunnel::command` already stated
and this now obeys: **remote mode owns how a server is reached.** A supervisor that
reinvents any part of it drifts from the flow it belongs to with nothing failing to
compile.

**A failure `diagnose` cannot name is still reported, once.** That gap is what hid the bug
above for the whole life of the exec transport: `Host key verification failed` matched none
of `diagnose`'s patterns, so the loop backed off to the ceiling and retried in silence for
the length of every session, under a banner that said `connected`. `should_say_it_cannot_connect`
is the whole decision — never carried anything, not yet said, past `QUIET_FAILURES` — and
it is a predicate rather than three inline conditions because `supervise` takes an owned
`Ui` and a test cannot read back what it printed. Unlike a named wall it keeps retrying
afterwards: an unrecognised failure cannot be told apart from a server that is slow to come
back.

**`RIABUILD_CHANNEL_SOCKET` outlives the channel, and the shim reports that as a state
rather than as an `errno`.** The variable is written once into the shell's environment when
the session opens; the channel is a live resource a laptop-side process owns and can end at
any moment — the sibling terminal that owned it exited first, a tmux window still open
tomorrow, a laptop that slept. Nothing reconciles the two and nothing can, because the
laptop is the side that connects. `client::unavailable` is where that is turned into
something a developer can act on, and it must stay a `Failure` with the remedy in `action`:
`channel status` renders the two apart, and a shim with only stderr gets both from
`Display`. The remedy is real — the socket path is per developer and per server, so a new
`riabuild remote` binds that same path and the shells already open recover with nothing
restarted.

The reason this earns prose rather than a one-line message: it is invisible in the worst
way. Claude Code's copy returns an OSC 52 escape unconditionally, so a dead channel leaves
**copying working and everything else broken**, and the half that still works reads as
proof the channel is fine. It was reported as two unrelated bugs. `channel status` says
that out loud, and the non-owner banner no longer claims `connected` for a channel it
neither started nor checked.

**There is no health probe, and adding one back would be a mistake.** The old design ran a
second short-lived `ssh` every thirty seconds to prove the forward carried traffic, because
a forward is a channel riding on the ssh session and can wedge while ssh believes itself
connected — keepalives run below it and cannot see that. With the exec transport the
requests travel over the ssh session's own stdio, so "carrying traffic" and "alive" are one
question, and `ServerAliveInterval` answers it on the same connection. What the probe fed —
the backoff reset — is now the request count `serve_pipe` returns.

Inside `archive`, `staging.rs` owns *how* a tree lands: unpack into a sibling
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
every environment shell opens with the account box, and the org's Claude settings, the
checkout's trust, and the plugins the checkout declares apply to every account, never just
the first.

**Three things riabuild wants cannot be settings, and each has its own home.**
`hasTrustDialogAccepted`, `hasCompletedOnboarding` and `defaultToAgentsView` are all
`.claude.json` state that `--settings` cannot express, so `claude_trust`,
`claude_onboarding` and `claude_agents_view` write them per account — and
`--exclude-dynamic-system-prompt-sections` has no key of any kind, so the launcher passes
it on the command line. Before adding anything to the dashboard's settings JSON, check it
is a settings key at all: one that is not gets served to every laptop and read by nobody.

**A settings value that names a path names it on every machine, so what it names goes in
`tools_root()`.** The org settings carry `node ~/.riabuild/claude-statusline.js`. Claude
Code runs that through a shell, so the `~` is the *account's* home wherever it lands —
and on a server that is the shared account, not the developer's namespace.
`claude_statusline` built its path on `root()` instead, which is the same directory on a
laptop and `~/.riabuild-remote/<member-id>` on a server: the script went into the
namespace, `node` was handed a path that did not exist, and remote mode had no status
line for its whole life. Nothing errored. A status line whose command fails renders as
*no status line*, so the only visible symptom was an absence, and `check()` reported
satisfied because the file it looked for was the one it had written.

Two lessons outlive the one file. **A path a *settings value* names cannot be built on
`root()`**, because the settings are org-wide and `root()` is per developer — the two are
only reconcilable where they coincide, which is exactly the machine every test was
written on. And a test that pins such a path must assert the **whole** path: the one
guarding this asserted the filename and the prefix separately, so both halves stayed true
when `root()` moved out from under the command and the test that existed to connect the
two repositories went on passing while they disagreed.

The live gate is `tasks::testing::ctx_on_a_server` — a `Ctx` whose `root()` and
`tools_root()` are different directories, which is the machine every unit test in that
crate had been missing. Reach for it for anything that has to be right on a server: the
laptop shape collapses the two roots into one directory, so a path built on either passes.
`e2e/remote/run.sh` looks for the script on the box as well, but that assertion is in the
block that **does not run yet** — see reason (2) in its header — so it is a recorded
intention rather than coverage, and reading it as coverage is the mistake that file's own
comments exist to prevent.

`claude_agents_view` is also the one task that **offers** rather than imposes. It writes
the key only where the account has no answer, because `/config` persists a developer's
`false` and a task that asserted `true` every run would silently overrule them on every
`riabuild`. Trust and onboarding are facts nobody wants undone; a view is not.

**Trust is the only gate Claude Code puts in front of a checkout's settings, and the
plugins ride on it.** `hasTrustDialogAccepted` — keyed by the checkout's *git root*, which
is why a subdirectory and a `.claude/worktrees/` worktree both inherit it — is what the
dialog writes and all it writes. Once it is set, the `extraKnownMarketplaces` and
`enabledPlugins` in the checkout's `.claude/settings.json` are installed by a background
pass with no second dialog. `claude_plugins` does that installation up front only because
the background pass lands *during* the first session and a plugin is loaded on the next
one — not because there is another prompt to suppress. Do not go looking for a
plugin-trust key; there isn't one.

Design: `../docs/superpowers/specs/2026-08-06-claude-accounts-design.md`.

## The Codex CLI

`codex_cli` installs `@openai/codex` with the Node riabuild owns and writes ten launchers:
`codex-1` … `codex-9`, each pinning `CODEX_HOME` to its own `~/.riabuild/codex/<n>`, and
`codex` for the first. Every one adds `--yolo`. riabuild does **not** sign anyone in: a
Codex sign-in is the developer's own OpenAI account, nothing riabuild brokers, and nothing
the onboarding path is blocked on.

**Nine profiles, because Codex really does keep sign-ins apart.** It stores credentials in
`$CODEX_HOME/auth.json` and nowhere else — no OS keychain, so nothing collides the way it
would for a tool that keyed one. Verified against 0.147.0: two homes hold two different API
keys at the same time, and `codex logout` in one leaves the other logged in.
`two_launchers_hold_two_independent_logins` pins that through the generated launchers
themselves, because nine scripts that each export a different directory and still shared an
account would pass every other test in the file.

**The nine exist from the first run** rather than being created on demand. Claude's
accounts are made by riabuild's own sign-in flow, which is what gives it something to
count; riabuild signs nobody in to Codex, so there is no moment at which it would learn a
developer wants a second one. Nine empty directories cost nothing, and `codex-3 login`
works the first time it is typed instead of failing on a `CODEX_HOME` that is not there.
That is also why there is no `riabuild codex new|delete|primary`: with a fixed set nothing
is ever created or renumbered, so `codex-3` and `~/.riabuild/codex/3` stay the same thing.

**The profile directories are numbered, not uuids.** Claude's are uuids because its
accounts can be deleted and renumbered, so *position* is the account number and the
directory name has to survive that. Codex's set is fixed, so the name can simply be the
number.

**Codex is a Node script, so everything that starts it has to carry riabuild's own Node.**
npm installs `bin/codex` as a symlink to a `codex.js` whose shebang is `#!/usr/bin/env
node`, which means starting Codex asks `PATH` for a Node before Codex gets a say. Both the
`check()` probe and the generated launcher name riabuild's Node explicitly; neither may go
back to `RunOptions::default()` or to a bare `exec`. Claude Code is the tempting precedent
and is not one — `@anthropic-ai/claude-code` ships a native `bin/claude.exe`, so
`claude_accounts` probing with a bare `RunOptions::default()` is correct *there*. That is a
fact about that package, not a pattern to copy, and copying it is how this bug was
written.

The cost of getting it wrong is paid entirely on machines that are not laptops. A
developer's own nvm or Homebrew Node answers the probe, so the whole thing passes locally
for a reason riabuild did not arrange; on a **managed server** riabuild runs under a
non-interactive SSH exec whose `PATH` is `/usr/local/bin:/usr/bin:/bin` and carries no Node
at all. `codex --version` exits 127 with `env: 'node': No such file or directory`,
`check()` reads that as "the Codex CLI is not installed", `apply()` installs it perfectly
well — `install_codex` was the one call that already set `PATH` — and the re-check says the
same thing again. That is the `apply()`-did-not-take-effect hard error, on every run,
forever, on a machine where Codex is installed and working. Shipped in #91 and fixed after
the first server ran it.

`the_launcher_starts_codex_where_the_machine_has_no_node` is the gate, and it runs the
generated script under a Node-less `PATH` rather than asserting on its text: a
`PATH="$codex_node_bin:$PATH"` exported on the wrong branch, too late, or with a variable
that expanded to nothing all read identically in the source.

**`CODEX_HOME` has to exist, not merely be named.** Codex refuses to start against a
directory that is not there — `Error finding codex home` — rather than creating one. So all
nine are part of what `check()` asserts, and each launcher recreates its own: the gap
between two riabuild runs is where a `rm -rf` lands, and the task would otherwise go on
reporting a satisfied machine while that `codex-<n>` refused to start. `check()` also
compares all ten launchers against what riabuild would write now, not just `codex` — a
developer who lives in `codex-4` would otherwise be the one person it cannot help.

**`--yolo` is a default, not an imposition, and it has to be.** Codex rejects the flag
twice (`cannot be used multiple times`) and rejects it beside `--ask-for-approval`
(`cannot be used with`), so a launcher that always appended it would make `codex --yolo`
and `codex -a on-request` — the two things a developer who knows this tool is most likely
to type — fail in the parser, naming a flag they never typed. The launcher scans its own
arguments and stands aside where the developer expressed an approval policy.

All of those are undocumented, read out of Codex 0.147.0, which is why `MIN_VERSION` names
that version and why the two `#[ignore]`d smoke tests run the generated launcher itself
rather than asserting on the text of a shell script. Run `cargo test -- --ignored` when the
floor moves. One trap they encode: `codex login status` reports on **stderr**, so a test
that reads stdout gets an empty string and fails for the wrong reason.

The Claude launcher's `unset SSH_CONNECTION SSH_CLIENT SSH_TTY` and its `WAYLAND_DISPLAY`
claim are deliberately **not** copied into it. Both are workarounds for behaviour read out
of the Claude Code binary; neither is a fact about Codex, and asserting them here would be
inventing an upstream behaviour rather than accommodating one.

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

**A message is never laid out at the call site.** No `\n` and four spaces inside a
format string: that fixes the indent and leaves the *wrapping* to the terminal, which
folds at column 0, so the second half of a sentence lands under the mark instead of
under the text. `ui::wrap` measures the terminal once (`TIOCGWINSZ` on stdout, floor 32,
ceiling 96, 80 when there is nothing to ask) and folds; a call site passes paragraphs.
`Detail::Prose` is folded and dimmed, `Detail::Verbatim` is printed whole and
emphasised — and which is which is the **caller's** to say. "A line with no spaces in
it" is the obvious rule and is wrong on the first thing riabuild prints this way: an
SSH public key is three words, and folding between any two of them hands the developer
something that is not a key.

**Anything that prints past an unfinished task has to end its status line.**
`Ui::working` leaves a line on screen with no newline and records its width; `applied`
and `unresolved` *cover* it, and everything else claims it through `end_status_line`.
`warn` did not, and warnings are on stderr where the `\r` that covers stdout cannot
reach — so a warning raised from inside a task rendered as
`◐ Authorised — installing the key  ▲ riabuild's key is already…`, with the task never
resolving. A downgraded task calls `unresolved`, which is `applied`'s `▲` counterpart
and carries the outcome plus its explanation as one block under one mark.

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
`gh auth login`. Not the environment shell, not `ssh`/`mosh`, not
`claude`: that is the developer's workspace, not riabuild's output. Not the clipboard
shim either, where riabuild is impersonating `xclip` and its stdout is a payload rather
than a page. Design:
`../docs/superpowers/specs/2026-08-12-subdued-child-output-design.md`.

## Shell integration

`bash --rcfile` **replaces** the user's `.bashrc`; zsh has no `--rcfile` and needs
`ZDOTDIR`; fish needs `XDG_CONFIG_HOME`. Generated rcfiles must source the user's real
config **first**, then apply riabuild's environment. Getting the first half wrong silently
destroys a developer's prompt, aliases, and history config, which reads as *riabuild broke
my shell*.

**The second half is not optional either, and exporting from the parent process does not
cover it.** The developer's config runs *after* riabuild set the environment, and
prepending to `PATH` is the most common line in a dotfile — Ubuntu ships it for
`~/.local/bin`, and nvm, pyenv, mise, asdf and conda each write their own. Any one of them
demotes `~/.riabuild/bin` from the front, and three separate things that document
"`~/.riabuild/bin` leads `PATH` inside the environment shell" as load-bearing stop working
at once: the `claude` launcher, the clipboard shims, and the `xdg-open` that carries links
to the laptop. Nothing errors. The developer's own `claude` simply starts instead of
riabuild's. `export BROWSER=firefox` in a `.bashrc` defeats the link channel the same way.

So `shell::environment_command` re-applies riabuild's environment at the bottom of every
generated rcfile — the same "riabuild gets the last word" shape the prompt hook already
uses. `PATH` is **moved to the front, never overwritten**: restating the parent's literal
value would discard whatever the developer's config legitimately added, which is the
opposite of the promise in each generated file's own header. Every other variable is
riabuild's outright and is simply re-exported.

The POSIX snippet is shared by bash and zsh; fish has its own because `PATH` is a list
there. The strip is a `tr`/`grep`/`paste` pipeline rather than a shell loop because one
string has to run under both shells, and zsh does not word-split an unquoted `$PATH` — the
obvious `for entry in $PATH` reads as a single element there and collapses the whole
variable to one directory.
