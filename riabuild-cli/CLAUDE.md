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

The exception is **stdio**. `riabuild-ui` writes with `println!`/`eprintln!`, and
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
why. `git_credentials::own_git_credentials`, called by `github_cli`'s `run_gh_auth` before
it hands over the terminal, removes the question rather than answering it.
So the test for a new subdued site is not "is its output untidy" but "does it ask" — plain
text and a wait for a person is fine, a full-screen prompt library is not.

Two things are synchronous because they are not IO, and are not exceptions to anything:
`riabuild-paths` computes paths without touching the disk, and `CommandRunner::which`
stats `PATH` candidates.

Tarball extraction is the one that genuinely is IO. `extract_tarball` writes through the
`tar` crate, which is synchronous and has no async spelling, so the whole of it is
`std::fs` — the tree it unpacks into a staging directory, the `rename` that installs it,
and the recursive removes that clear up after a failure. Making the directory calls around
a synchronous writer async would be theatre, which is why they are not.

**Where the blocking work is not `tokio::fs`-shaped, it goes on the blocking pool.**
Opening a directory by descriptor and `fstat`/`fchmod`-ing it, `access(2)`, and the
blocking `lock()` a contended file lock falls back to are POSIX calls tokio has no async
wrapper for; each is run through `tokio::task::spawn_blocking`, never inline on the
reactor thread and never `block_in_place` — that one needs `rt-multi-thread`, which a
current-thread runtime is not, and it borrows a worker rather than leaving it to the
dedicated pool.

Note that `tokio::fs` is `std::fs` on a blocking threadpool: no portable async file API
exists. "Current-thread" describes the reactor, not the process, and the binary does have
threads. Closures cannot be async, so `and_then`/`unwrap_or_else` chains around IO have to
be unrolled into `match` or `let else` rather than kept for tidiness.

**A repository is a `Repo`, never a `String`, and a task never reads `org.repo_slug`.**
`api::Repo` is the only thing that may name one, because the value reaches `gh repo clone`
argv *and* a directory name: a leading `-` is an option rather than a repository, and `..`
puts a checkout — with the brokered `.env` files `env_local` writes into it — outside the
directory riabuild chose. It arrives from a developer at a prompt as well as from the
dashboard, and `org.update` stored that field with no check at all until the picker existed.

`org.repo_slug` is the **default** the picker offers and nothing else. What a run is about
is `Ctx::repo`, set by the picker or by `--repo`; a task that reaches for the org's slug
instead will clone one repository and provision another, on a machine where every test
still passes because the two are the same value until somebody picks.

**A remote run asks both of its questions on the laptop, before the first `ssh`.** Which
server, then which repository, back to back — and the answer travels to the server as
`--repo`, which is the flag that already existed for naming one and which the server's own
picker already stands aside for. It used to be asked on the far side, by the server's own
riabuild over `ssh -t`, which put the two halves of one run at opposite ends of a host key,
an `ssh-copy-id`, an install and a session mint: the developer committed to provisioning a
machine before being asked what it was for.

Two things hold that up and both are load-bearing. The laptop must **write nothing about
itself** — `repo::pick::choose` records what it settled on in this machine's `config.json`,
so `riabuild remote gpu` would otherwise leave the laptop working on whatever the server
was told to. `repo::pick::offer` is the same question with none of that, and where the
answer goes instead is `Record.repo`, beside the server in `remotes.json`. And that field
is not bookkeeping: it is the memory the *server's* picker used to keep, and a laptop that
always passes `--repo` never lets the server offer it, so pressing Enter would silently
move a server onto whatever the laptop was doing. Design:
`../docs/superpowers/specs/2026-08-26-remote-repository-first-design.md`.

Where the checkout *goes* is still asked on the server, by the server, and that is correct
rather than an omission: `project::choose_dir` is a question about a filesystem this laptop
cannot see.

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
`riabuild-keychain`. Infisical tokens are short-lived, brokered per use, and handed to
`infisical` in that one process's environment — never written down. That holds for the
developer's own `infisical` as well as for `env_local`'s: `~/.riabuild/bin/infisical` is
not an `exec` line but a hand-back to `riabuild internal infisical`, which brokers one
credential per command. `infisical login` is what this exists instead of.

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

**Remote mode is used from more than one window too, and it took a second pass to say so.**
"One person connecting to one server in one or more terminals" is the *intended* usage of
`riabuild remote`, not a corner of it, and every place the crate reasoned about a second
connection reasoned about a colleague under a shared Unix account instead. Three more
locks came out of writing that down, each an `flock` for the reason above:

| Lock | Stops |
|---|---|
| `agent/<server-hash>/<pid>/run.lock` | two windows unlinking each other's issued-key `ssh-agent` socket, and either one's teardown deleting the directory the other serves from |
| `remote-sessions/<server-hash>.lock` | two windows both minting a server session, which orphans one — a live 90-day session no `remote forget` can name |
| `remote-windows/<server-hash>/<pid>.lock` | `remote forget` revoking a session, unauthorising a key and clearing a namespace that another of this laptop's terminals is sitting in, in silence |

The last is a *count*, not a mutex: nothing waits on it, and `forget` warns rather than
refusing — it is a destructive command typed by name, usually because that server has gone
wrong, and it also runs unattended from `shared::reconcile`. Liveness is always the
kernel's answer to "does anybody hold this", never a pid and a `kill -0`: a recycled pid
says "still connected" about somebody else's `vim`, and an age cap cannot be used because a
remote session outliving a day is the normal case. Design:
`../docs/superpowers/specs/2026-08-28-many-windows-one-server-design.md`.

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

**riabuild owns every tool it depends on.** Node, pnpm, Claude Code, the Codex CLI, Grok
Build, `gh`, `infisical` and `ngrok` are downloaded, verified against a digest, and kept
under
`~/.riabuild/`. Nothing on the developer's `PATH` is trusted, and no task shells out to a
package manager to install a dependency. Run them through `ctx.gh()`, `ctx.infisical()`,
`ctx.ngrok()` and `ctx.grok()` rather than by name: during provisioning `~/.riabuild/bin`
is not on
`PATH`, so the bare name finds a binary no `check()` verified, or nothing at all.

**A tool becomes owned by being a row in `owned_tool`, never by a fourth copy of the four
steps.** `crates/tasks/src/owned_tool.rs` is one table, and a row carries the pinned
release, the digest the download is verified against, the environment the `--version`
probe runs in, and what `~/.riabuild/bin` gets — so downloading, verifying, landing the
tree under `~/.riabuild/<tool>/<version>/` and putting something on the developer's `PATH`
are properties of the table rather than of whoever wrote the task. They used to be four
copies of those steps, and copies drift: only ngrok checked its own shim, so a deleted
`bin/gh` reported a satisfied machine while the shell went on finding whatever `gh` the
laptop already had. Where rows differ they differ as **data** — two of the shims are not
`exec` lines. ngrok's fetches the team's authtoken per invocation, which is the whole
reason that token lands on no filesystem; infisical's hands the developer's command back
to `riabuild internal infisical`, which brokers a short-lived credential for it, for the
same reason and one more: `infisical login` would write one down.

`infisical` and `ngrok` are nothing but a row, so the row *is* the task. `github_cli` and
`grok_cli` compose a row and keep their own `Task`, because signing the developer in and
asking GitHub about their membership, and making nine profile directories, are work that
is genuinely theirs rather than a field the table is missing. The Codex CLI is
deliberately **not** a row: it is an npm package installed with the Node riabuild owns, so
it has no release, no asset and no digest of its own, and a row describing it would be
empty in every field this table exists for.

ngrok and Grok Build are the two whose digests are **not** published by their own
projects, and `tools::Checksum` is where that shows up. `Published(urls)` is the normal
case — gh and infisical fetch the checksum files their releases carry. `Pinned(digest)` is
those two: Equinox serves one floating ngrok per platform with no checksum anywhere, and
xAI serves an honestly-versioned Grok Build with no checksum anywhere, so
`packaging/ngrok/mirror.sh` and `packaging/grok/mirror.sh` republish the bytes we verified
as `Clubria/riabuild` releases and the digests are constants in this repository. Reaching
for `Pinned` for a tool that *does* publish digests would freeze a value that moves with
every upstream release; reaching for a server-supplied digest would hand riabuild-web the
choice of what executes.

**pnpm is in neither arm and is not a row in `owned_tool` either**, because its version
comes from the checkout at runtime rather than from a constant here — `tasks::toolchain`
owns it. It publishes no checksum file, so it takes the third route: the **npm registry**,
whose `dist.integrity` is a sha512 the publisher recorded over the stored tarball and is
served with no API budget to run out of. Do not reintroduce `api.github.com` to get a
digest, however tempting the per-asset one GitHub records looks. Sixty unauthenticated
requests an hour *per address* is a ceiling one office behind one NAT reaches, and when it
does, provisioning fails for all of them at once; the whole of `crates/fetch/src/tools/`
now has no route to that host, deliberately.

**pnpm is also the one tool riabuild installs as a *script* rather than as a binary, and
that is forced rather than chosen.** What riabuild unpacks is the unscoped `pnpm` package —
`bin/pnpm.cjs` with `dist/` beside it — started by `~/.riabuild/bin/pnpm`, a shim that
`exec`s riabuild's own Node against it. Neither of pnpm's platform executables can run on
the machines this provisions:

- `@pnpm/linux-x64` is `NEEDED: libatomic.so.1`. That file is not in `debian:bookworm-slim`,
  `debian:12`, `ubuntu:22.04` or `fedora:41` — it arrives with a toolchain, and a machine
  that already has a toolchain on it is not the machine riabuild is pointed at.
- `@pnpm/linuxstatic-<arch>` reads like the answer and is not: it is built against **musl**
  and its interpreter is `/lib/ld-musl-x86_64.so.1`, so on a glibc distribution it does not
  fail to find a library, it fails to start at all.

Node's own binaries link neither, which is what makes the failure so hard to read: Node
installs and answers `-v`, `check()` gets past it, and pnpm exits **127** beside it — which
`reported_version` correctly turns into "pnpm is not installed yet". `apply()` then
downloads 146 MB, unpacks it perfectly, and the re-check says the same thing. That is the
apply-did-not-take-effect hard error, for ever, on a machine where nothing is wrong except a
library nobody named. It is the Codex CLI bug below, one missing thing over, and
`e2e/remote/run.sh`'s Debian container is where it surfaced.

Two rules fall out of it and both are load-bearing. The shim names **both** paths
absolutely — `shims::node_shim`, pinned by
`the_shim_starts_pnpm_with_riabuilds_own_node_by_absolute_path` — because `bin/pnpm.cjs`
opens `#!/usr/bin/env node` and a server reached by a non-interactive SSH exec has a `PATH`
of `/usr/local/bin:/usr/bin:/bin` with no Node in it. And the version probe is
`node <entry> -v` rather than `<binary> -v`, which is why `toolchain` has
`reported_script_version` beside `reported_version` rather than one function that guesses.

The fix for this is never `apt-get install libatomic1`. `e2e/remote/Dockerfile` says so by
name, because one line there turns CI green and leaves every developer on a stock server
hitting the same silent 127 — and "a provisioner that needs a package manager already set
up cannot be the first thing a developer runs" is the whole of why riabuild exists.

Grok Build's asset is the one that is **not an archive** — xAI serves a bare executable —
so `archive::Kind::Raw` reads it straight through and `mirror.sh` renames it to `.bin`
rather than repacking it. Repacking would make the pinned digest describe our own output
rather than upstream's. `Kind::Raw` is spelled as an explicit suffix and never inferred
from "an extension I do not recognise", which would install a `.pkg` or a `.deb` as though
it were a binary and fail when the developer ran it.

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

**Self-update asks what owns the binary, never what is installed.** On Linux `update.rs`
runs `dpkg -S` and then `rpm -qf` against the running executable; on macOS it asks
Homebrew, and Homebrew is asked two questions because either alone is the wrong answer.
`brew --prefix` names the one tree brew installs into — the Cellar and the `bin` symlinks
into it both sit under it — so an executable that is not under it was put there by
something else; and `brew list --formula riabuild` says whether there is a formula to
upgrade at all. It is `brew --prefix` rather than a hardcoded `/opt/homebrew` because
Apple silicon and Intel disagree about that path and a developer may have moved it.
Anything that answers no is `Unmanaged`. A Fedora machine can have `apt` on it, and a
riabuild built with `cargo` is owned by nothing — `sudo apt-get install riabuild` there
installs a *second* riabuild elsewhere and leaves this one in place, so every upgrade
reports success and nothing changes. That case prints the command and never sudoes.

macOS used to answer `Homebrew` with no probe at all, which is that same failure on the
platform where a `cargo build` riabuild is most common, since this is the repository
developers work on from Macs: `brew upgrade clubria/tap/riabuild` poured a second copy
under the prefix and left this one running, for ever, reporting success every time. The
platform reaches `strategy_on` as a **parameter** for the reason `keychain::select` does —
with `cfg!` inline the other branch is compiled out of the test binary, so each test
asserted only whichever half its host happened to be.

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
| `theme` | the Clubria palette, by role, and the depth ladder under it — expressed in ratatui's `Color`/`Style`/`Modifier`, so the printed line and the drawn frame share one palette | `ratatui-core` |
| `version` | riabuild's own `VERSION`, and version parsing and comparison | — |
| `fetch` | `download` (where bytes come from, and whether they match a published digest), `archive` (unpacking what download fetched, and `staging` for landing a tree atomically), `tools` (the gh, infisical, ngrok and Grok Build releases riabuild owns) | ui |
| `ui` | output, prompts, and the `Failure` every error becomes; `art` is the riabuild mark and the banner | theme, version |
| `runner` | `CommandRunner` — all subprocesses. `subdue` is the line filter a subdued child's output goes through; `pty` is the terminal it gets instead of riabuild's own | theme |
| `paths` | path resolution (trait), `config` (`~/.riabuild` and state), `filelock` (the lock both are read and written under) | ui |
| `keychain` | secret storage: the trait, the two platform CLIs, the file store for machines with no keyring, and `keyring_answers` — whether a Secret Service actually replies | paths, runner, ui |
| `api` | the riabuild-web client: sessions, org configuration, brokered secrets | runner, ui |
| `gh-session` | where the GitHub config dir goes, how it is created safely against a co-tenant, and how long it lives | paths, runner, ui |
| `channel` | the laptop channel: clipboard and browser over an SSH exec session. `mux` frames many shim connections onto one pipe, `pump` is the server end that binds the socket and relays, `agent::pipe` is the laptop end; `socket` decides where that socket lives and refuses one that is not ours; `supervisor` keeps the connection up | gh-session, paths, runner, ui |
| `tasks` | the `Task` trait, the registry, the DAG runner, one file per task; `owned_tool` (the table of tools riabuild downloads whole — one row per tool, carrying its release, digest, probe and shim); `accounts` (the Claude Code accounts), `repo` (which repository a run is about: the `gh` listing, the box, and the picker), `shell` (zsh, bash, fish), `shims` (`~/.riabuild/bin` generation, and `launch` — what those one-line launchers do, in Rust), `scope` (laptop vs. server) | all of the above |
| `remote` | remote mode: identity, host-key trust, authorising a key, installing the server's own binary, minting its session, seeding a GitHub sign-in, and the mosh/ssh handoff. `askpass` answers the password prompt when the key cannot sign in; `pick` is the prompt a bare `riabuild remote` puts, and `render` the box it and `list` show; `repo` is the question straight after it — which repository, asked here rather than on the server, and forwarded as `--repo`; `shared` folds the team's servers in from riabuild-web on every run; `ssh` is the one place an `ssh` invocation is composed, and all thirteen call sites go through it. `channel` is where the clipboard channel is attached to a session — `lease` decides which of this laptop's sessions serves it, `hold` waits for a turn and takes one | all of the above |
| `harness` | what to run for each agent harness and how to read what it says. `claude`, `codex` and `grok` build one turn's argv and decode that harness's NDJSON. Starts nothing and reads nothing | — |
| `agents` | the `riabuild agents` window. `store` is the sessions on disk — records, spools, locks; `turn` is what `internal agent-turn` runs; `app` is what is on screen and how an event changes it (pure); `draw` turns that into ratatui lines and then into a frame | harness, paths, runner, theme, ui |
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

**Nothing riabuild writes into `$PATH` contains logic. Every generated script in
`~/.riabuild/bin` is a shebang, some comments, and exactly one `exec`.** The decisions
those files used to make — which harness binary, which `PATH`, which profile, which flags,
which checkout the agents view opens on, whether the developer already named an approval
policy — are `riabuild_tasks::shims::launch` and the three `handoff` functions beside it,
reached through `riabuild internal launch <harness>`.

This is an architectural rule, not a tidying preference, and the reason is what shell *is*:
a language with no type checker, no test that runs in CI without spawning a subprocess, and
a parser that turns a mistake into a **different working program** rather than into an
error. The `claude` launcher alone was ninety lines of it, and its own comments record the
class of bug it kept nearly reintroducing — a `PATH` strip whose `grep -vxF` loses `-x` and
empties a developer's `PATH` of everything under their home; a path with a space in it
splitting back into two arguments after `${x:+--cwd "$x"}`; a `set --` branch leaving on a
flag that turns `claude` into the background-agents *listing*. Each was prevented by a
comment, which is a rule enforced by whoever reads it next.

Three properties are kept rather than traded away. The resolved values are **still in the
file**, on the exec line, so `check()` comparing a launcher against what riabuild would
write now still catches one naming last week's Node or a deleted account — moving them into
riabuild's own state would make every launcher byte-identical and that comparison
worthless. riabuild is **still named in full**, per the rule below. And it is **still an
`exec`**: `CommandRunner::exec_replacing` is `execvp(2)`, so the developer's shell waits on
one process, `Ctrl+C` reaches the harness, and `jobs` shows what they started. Spawning
instead would park a riabuild between the terminal and the session for its whole life.

`launch` is dispatched in `main::run` **before a `Ctx` exists**, beside `askpass` and
`channel`, and that placement is load-bearing: it runs every time a developer types
`claude`, and the shell script it replaces read no config, opened no socket and printed
nothing. `every_generated_script_is_a_single_exec` is the gate on the shape, and
`every_generated_launcher_parses_back_into_the_plan_that_wrote_it` is the gate on the seam
— `riabuild-tasks` writes those files and has no parser, `crates/cli` parses them and has
no generator, so a flag renamed on one side compiles perfectly and fails on a laptop.

`internal ngrok` is the same move where it matters most. That shim was the one piece of
shell in `bin/` that handled a **secret**: `NGROK_AUTHTOKEN=$("…/riabuild" internal
ngrok-token)`, a command substitution whose stdout was a credential, into a shell variable,
in a process that then went on to `exec`. The token is now fetched by the process that
*becomes* ngrok. `internal ngrok-token` stays only because a shim written by an older
riabuild is still on disk until the next provisioning run rewrites it.

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
`../docs/superpowers/specs/2026-08-07-clipboard-channel-design.md`, whose transport
sections were superseded by
`../docs/superpowers/specs/2026-08-13-exec-channel-transport-design.md` — read both, in
that order, because the older one still describes the `ssh -N -R` reverse forward that
the paragraph below says must never come back.

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
supervisor composes.** `supervisor::Tunnel` takes `options: Vec<String>` — what
`ssh::Ssh::options_only` hands it, off the same builder behind the setup run, the mosh
bootstrap and the developer's shell — and `ssh_args` adds only what is its own: `-T`, the
keepalives, `BatchMode=yes`. `identity::ssh_options` is `pub(crate)` and reached through
`Ssh` rather than called at a call site, so the base list is not something a caller can
half-assemble. It used to take a `port` and an `identity` and build `-p`/`-i` itself,
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
is the whole decision — never once connected, not yet said, past `QUIET_FAILURES` — and
it is a predicate rather than three inline conditions because `supervise` takes an owned
`Ui` and a test cannot read back what it printed. Unlike a named wall it keeps retrying
afterwards: an unrecognised failure cannot be told apart from a server that is slow to come
back.

**A named wall stops the supervisor for the rest of the session, so `diagnose` is matched
against why ssh gave up rather than against everything ssh wrote.** `supervisor::decisive`
drops two kinds of line before any pattern is tried, and both carry words that read as
decisive while being nothing of the sort. `Warning: Identity file … not accessible: No such
file or directory.` is ssh saying it will offer one key fewer and then carrying on — left
in, it turns every ordinary disconnect underneath it into "the server has no `riabuild
channel pump` to run", permanently, on a server that has one. And **a hostname that will
not resolve is a laptop that has just woken up**, which is the single most common way this
connection fails and the one case that must always be retried, since retrying is the whole
of how the channel survives a closed lid. Resolvers disagree about the words, and one of
them — `Host not found` — really does contain a pattern above, so the line is dropped by
what it is about rather than by which spelling it used. Only the *matching* is narrowed:
`Failure::detail` still carries the whole of stderr.

**"Never connected" is not "never carried a request", and conflating them is a message that
lies.** Those were one flag. A connection carries a request only when somebody pastes, so on
a link that drops and rebuilds — which is the whole reason the developer is on mosh — an
idle channel that came up perfectly every time reported that it could not reach the server,
four rebuilds in. `serve_pipe` returns `Served { requests, keepalives }` and `connected()`
is either; `requests` alone still means "somebody's paste worked" and is not what a message
about reachability may be built on. The keepalive is what gives an idle connection anything
to say for itself, which is why this and the pump's own liveness are one change.

**A socket another of this laptop's sessions is serving is not a wall, and is not a
message.** `pump::ALREADY_SERVED` comes back from a server the `ssh` reached perfectly, and
it is the one answer that *proves* the channel is up: the shims in this session's own shell
are already pasting through the pump being complained about. `supervise` answers it with
`Outcome::AlreadyServed` — before `diagnose`, without a word, without a retry — and `hold`
hands the lease back and stands by.

It used to fall through to the unrecognised-wall path, and the fix for *that* was a second
sentence ("another session on this server is still holding the channel"), which was a true
description of a stale pump and a lie in the far commoner case: one developer with two
terminals into one box, reading **"paste is off"** while pasting. Recognising the refusal
and treating it as a failure at all was the error. Three things were wrong and only one was
the wording — it retried, which is an `ssh` and an authentication against somebody's `sshd`
every few seconds for as long as two windows are open; and `bind`'s own remedy said *close
the other riabuild session*, which is advice to break a working session to fix one that was
never broken.

`ALREADY_SERVED` is **one constant used by both ends**, matched as a substring, and it is a
wire format: the laptop and the server can be a release apart, so it is deliberately the
phrase the older wording also contained. Reword it on one side only and nothing fails to
compile — the false alarm simply comes back.

What is still true is what the pump's keepalive below is for: a pump that outlived its
laptop holds the socket for real, and paste really is dead until it gives it back. `hold`
counts the bounces and says one thing after about ninety seconds — never "paste is off",
because from the laptop riabuild cannot tell that case from a working sibling. Only
`riabuild channel status`, which asks the socket itself, can.

**Serving the channel is a lease, and every session keeps asking for it.** One of this
laptop's sessions to a server serves the channel — a second pump would find the first one's
socket live and be refused — and the rest *stand by*, asking every five seconds whether the
lease has fallen free, for as long as their shells are open. `remote::channel::hold` is
that loop and `remote::channel::lease` is the lease, an `flock` on
`~/.riabuild/channel-sessions/<hash>/owner.lock`.

**The lease is an optimisation; the server's socket is the authority.** They are keyed
differently and cannot be made to agree — the lease by the login target *as typed*, the
socket by the server's `<home>/.riabuild-remote/<member-id>` — so two windows reaching one
machine as `build-01.fly.dev` and `10.0.0.5` hold a lease each over one socket, and on
every handoff the standing-by window takes the lease before the old pump has finished
dying. Do not try to re-key the lease onto the socket path to close that: two *different*
machines can have identical socket paths, and one of them would then stand by for ever.
The loser self-corrects instead, which is what `Outcome::AlreadyServed` is for. The bounce
waits on `supervisor::backoff` rather than the five-second standby poll, because asking the
lease costs one `flock` on a local file and asking the socket costs an `ssh`.

It replaces a `Claim`: a `sessions/<pid>` marker per session and one question asked once,
*am I the first?*. A session that answered no started nothing and never asked again, so
when the owning session's laptop-side process ended the survivor sat there for the rest of
its life naming a socket path that was correct and unbound — paste, image paste and
`xdg-open` all dead, with riabuild running in that very terminal. Two terminals and a
closed lid is not an exotic case, and the old banner *documenting* the limit ("it ends when
that one does") did not stop it being reported as a bug.

**The lease is an `flock` and must stay one.** A pid in a file is a claim somebody else has
to check, and every way of checking it is wrong somewhere: a marker outlives its process,
so it needs a sweep, and a sweep that runs only at startup cannot see an owner that dies
later; `kill -0` on a recycled pid says "alive" about the wrong process; and `gh_session`'s
age cap, which is how that file covers recycling, cannot be used here because a remote
session outliving a day is the normal case. The kernel releases an `flock` when the holding
process exits however it exits, so "the owner has gone" and "the lock is free" are one
question and one syscall. `FileLock::try_acquire` is the try-only sibling `acquire` did not
have: a session standing by must never *queue*, or it parks a blocking-pool thread for as
long as its sibling's shell is open and then takes the channel over at the moment that
shell exits whether or not it is still there to serve it.

What this does **not** move: the laptop is still the side that connects, so when the last
session to a server ends, a new `riabuild remote` is the only thing that brings the channel
back. `hold` also stops standing by after a wall `supervise` names — two sessions each
re-taking a lease they cannot use is a pair of laptops authenticating against a wall in
turn, for ever.

**`RIABUILD_CHANNEL_SOCKET` outlives the channel, and the shim reports that as a state
rather than as an `errno`.** The variable is written once into the shell's environment when
the session opens; the channel is a live resource a laptop-side process owns and can end at
any moment — a tmux window still open tomorrow, a laptop that slept, or every one of this
laptop's sessions to that server having ended. Nothing on the server reconciles the two and
nothing can, because the laptop is the side that connects. `client::unavailable` is where that is turned into
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
the backoff reset — is now what `serve_pipe` returns about the connection it just lost.

**`ServerAliveInterval` only measures the end that started the connection, and the pump has
a keepalive because of it.** `sshd` ships with `ClientAliveInterval 0`, and a TCP connection
whose peer stopped acknowledging looks exactly like an idle one for as long as the kernel
retransmits — a quarter of an hour, unbounded if the peer is wedged rather than gone. So a
laptop on a flaky link left a pump *alive on the server*, still bound to `channel.sock`,
relaying into a pipe nothing would ever read. One cause, three unrelated-looking symptoms:
every paste and every `riabuild channel status` waited out the reply timeout and failed;
every pump the reconnecting laptop started was refused with `already serving`; and the
supervisor said it could not reach a server it was reaching every time.

The pump now sends one frame every `KEEPALIVE_INTERVAL` (15 s) and returns after
`KEEPALIVE_DEADLINE` (45 s) of silence — the laptop's own `ServerAliveInterval` and
`ServerAliveCountMax`, measured from the other end — unbinding the socket as it goes. This
is **not** the deleted health probe: it costs no second SSH connection and asks the server
nothing. It is `mux::KEEPALIVE_ID`, a reserved id carrying **no payload**, which is what
keeps "the pump is a relay and never a parser" true of it — it names no operation and reads
no answer, it only obliges a frame back, and `serve_pipe` answers every frame including one
it cannot parse. Measure it against `tokio::time::Instant`, never `std`'s: the sleeps beside
it are tokio's, and a deadline on the other clock is a test that cannot fail.

**The supervisor is the one thing that prints beside a shell rather than in front of one, so
it speaks on a status bar.** In a remote session the terminal is in raw mode — `\n` drops a
row without returning to column one — and mosh and Claude Code are painting it, so a folded
warning printed there arrives as a staircase and stays in the middle of somebody else's
screen. `riabuild_ui::StatusBar` puts one line on row two (mosh owns row one) over
`/dev/tty`, with the cursor saved and restored; `supervisor::StatusLine` redraws it on a
tick because nothing announces a repaint underneath, and `remote::channel` owns its
lifetime because the session's end is when the line comes off. A bar carries one line, so
the detail and the remedy stay in `riabuild channel status`. With no bar — every non-remote
run, `--quiet`, every test — `report` prints exactly as it always did.

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

**`riabuild paths` prints those directories, which is not the same as exporting one.**
`config_dirs` lists every account against its `CLAUDE_CONFIG_DIR`, every Codex and Grok
Build profile against its `CODEX_HOME` and `GROK_HOME`, and riabuild's own tree beneath
them — a page a developer reads, inherited by no process, so the paragraph above is
untouched. It exists because a uuid riabuild chose is not something a developer can guess:
without it, "which directory is my second login in?" is answered by opening a generated
launcher. The one thing it asks rather than computes is who is signed in to each account,
because a column of uuids with no logins beside it does not answer the question that was
asked. Nothing there stats a directory: a path is what riabuild *would* point a tool at,
on a machine nothing has provisioned as much as on one that is set up, and whether the
machine is in shape is `riabuild --check`'s question.

**Three things riabuild wants cannot be settings, and each has its own home.**
`hasTrustDialogAccepted`, `hasCompletedOnboarding` and `defaultToAgentsView` are all
`.claude.json` state that `--settings` cannot express, so `claude_trust`,
`claude_onboarding` and `claude_agents_view` write them per account — and
`--exclude-dynamic-system-prompt-sections` has no key of any kind, so the launcher passes
it on the command line, on every launch but the bare interactive one that takes the
agents view instead. Before adding anything to the dashboard's settings JSON, check it
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

**The key is not what opens the view, though, and never was.** Claude Code reads
`defaultToAgentsView` only when the raw command line holds nothing but debug flags, and
every launcher passes `--settings` — so on a Clubria laptop that key has never decided
what `claude` opened on. The launcher reaches the view by the bare `agents` positional,
which is tested after the recognised options are stripped and which ignores the key. Two
consequences worth holding on to. `--exclude-dynamic-system-prompt-sections` cannot ride
along: it is not stripped, so the pair drops through to the *background-agents
subcommand* and `claude` prints a list instead of opening a session — which is why a bare
launch gives the flag up, and why it loses nothing by doing so (Claude Code does not
carry it into sessions dispatched from the view either). And the developer's `/config`
answer no longer applies through the launcher, so `CLAUDE_CODE_DISABLE_AGENT_VIEW` is
their way out and the launcher honours it — without that guard a disabled view would not
degrade to a session, it would exit 1 and take the `claude` command with it.

**`--cwd` opens that view on a checkout, and it is the one flag only the bare line can
carry.** It belongs to the `agents` *subcommand* rather than to `claude` — `claude --cwd
<path> mcp list` is "unknown option" — so it sits after the positional, and a copy of it on
the branch that forwards a developer's own arguments would not scope anything: it would
break `claude -p`, `claude --resume` and `claude-2 auth login` on every laptop at once, in
Claude Code's own parser. Which makes it the mirror of
`--exclude-dynamic-system-prompt-sections`, the one flag only the *other* line can carry,
and the reason "is it stripped before the positional is tested?" is the wrong question to
ask of it. An option *after* `agents` costs the view nothing.

Two things about what it does. It scopes the session list to that path — which is what its
`--help` line says and all it says — *and* it becomes the working directory the view
reports and dispatches from, which is the half worth having: a `claude` typed in a home
directory used to open a list of every session on the machine from every checkout. And it
is a **floor rather than a move**, because Claude Code keeps the process's own working
directory when that directory is inside the one named here — so `claude` from
`<checkout>/riabuild-cli`, or from a `.claude/worktrees/` worktree under it, still opens
where the developer stands. Passed only where the resolved checkout is on disk: a path
that is gone does not error, it opens a view onto an empty list naming a directory nobody
has, which is worse than the view the launcher wrote before the flag existed. Verified
against 2.1.235 and pinned by `the_view_cwd_is_an_agents_option_and_only_an_agents_option`,
which asserts both halves — accepted after the positional, rejected before it.

**Which checkout is resolved per launch, by `$PWD`, never baked into the launcher as one
repository for the whole machine.** `UserConfig::repos` holds every checkout this machine
knows about, keyed by `owner/repo`, and `shims::claude::known_checkouts` hands the whole
map to the launcher — not just `Ctx::project_dir`'s answer for the run that happened to
generate it. Each becomes a `--checkout` on the launcher's exec line, longest path first,
with the run's own default as `--default-checkout` for a developer standing in neither,
and `shims::claude::checkout_for` picks between them **at launch time**. The floor above
still holds per checkout — `Path::starts_with` matches the checkout root and everything
beneath it — so which repository "it" resolves to is a question answered at the moment
`claude` runs, not one the Rust code can answer once and freeze into the file.

`checkout_for` used to be a `case "$PWD" in "$path"|"$path"/*)` block in the generated
shell script, and moving it into Rust is strictly better in a way worth naming: the shell
pattern was right only because of where the `/` sat in it, so `~/Clubria/payments` did not
swallow `~/Clubria/payments-legacy` by one character's grace. `Path::starts_with` compares
whole components and cannot get that wrong.

This is what makes a developer who works in two Clubria checkouts — `riabuild` and, say, a
product repository, each in its own terminal — get `--cwd` right in both, rather than in
whichever repository `riabuild` was *most recently run against*. Before
`known_checkouts`/`checkout_for` existed, `--cwd` was one path, chosen by
`Ctx::project_dir` at the moment the launcher was last written: a developer standing in
`riabuild` was moved to the other checkout the instant a `riabuild` run against it
regenerated the script, because the floor only keeps you where you stand when the single
path the launcher knows is the one you are in. Design:
`../docs/superpowers/specs/2026-08-18-repository-picker-design.md`'s "Out of scope" line
ruling this out is superseded — see the addendum at its foot.

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

`the_launcher_carries_riabuilds_own_node` is the gate, and it asserts on the environment
`shims::codex::handoff` decides — which is now the whole answer. It used to have to *run*
the generated script under a Node-less `PATH`, because a `PATH="$codex_node_bin:$PATH"`
exported on the wrong branch, too late, or with a variable that expanded to nothing all
read identically in the shell source. None of those three failures has a spelling in Rust.

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
that version and why the two `#[ignore]`d smoke tests run a **real Codex** with the
arguments and environment `handoff` decides. They drive `handoff` rather than the generated
launcher deliberately: what is under test there is Codex, not riabuild's `exec` line, and
running the launcher would need a built riabuild binary to exec into while asking the same
question one process further away. Run `cargo test -- --ignored` when the floor moves. One
trap they encode: `codex login status` reports on **stderr**, so a test that reads stdout
gets an empty string and fails for the wrong reason.

The Claude launcher's clearing of `SSH_CONNECTION`, `SSH_CLIENT` and `SSH_TTY` and its `WAYLAND_DISPLAY`
claim are deliberately **not** copied into it. Both are workarounds for behaviour read out
of the Claude Code binary; neither is a fact about Codex, and asserting them here would be
inventing an upstream behaviour rather than accommodating one.

## Grok Build

`grok_cli` downloads xAI's coding agent from riabuild's own mirror and writes ten
launchers: `grok-1` … `grok-9`, each pinning `GROK_HOME` to its own `~/.riabuild/grok/<n>`,
and `grok` for the first. Every one adds `--permission-mode bypassPermissions`. riabuild
does **not** sign anyone in: a Grok sign-in is the developer's own xAI account.

Read as a diff against the Codex section above — the shape is deliberately the same, and
the four places it differs are the interesting ones.

**It is a static binary, not a Node script.** The Linux build is a `static-pie` ELF, so
neither the `--version` probe nor the launcher needs riabuild's Node on `PATH`, and
`depends_on()` is empty rather than naming `toolchain`. Do not copy the `PATH` line
`shims::codex::handoff` puts in front of the Node directory here on the grounds that it
looks symmetrical: it would be carrying a Node for nobody, and
`the_launcher_carries_no_node_and_no_claude_workarounds` fails if it appears. This is also why `Ctx::grok()` is an `owned_tool` like
`ctx.gh()` — always an absolute versioned path — rather than the Node-relative path with a
bare-name fallback that `Ctx::claude()` and `Ctx::codex()` return.

**`GROK_HOME` does not have to exist first, and the nine are created anyway.** Codex
hard-fails on a missing `CODEX_HOME` (`Error finding codex home`), so creating all nine
repairs a machine that would otherwise be broken. Grok Build creates one that is not there.
riabuild still creates the nine, so that "nine accounts" is a state `check()` can assert
rather than a promise that comes true the first time each launcher happens to be run. The
same upstream behaviour is why the version probe **names** a `GROK_HOME` rather than
leaving it unset: an unset one does not merely read the developer's `~/.grok`, it brings
that directory into existence.

**The bypass is a flag, and it has to be.** Grok Build resolves the launch mode as *CLI
beats `[ui] permission_mode` beats remote*, so the command line is the only spelling that
cannot be silently overridden by a value already on disk — and `config.toml` is a file the
developer owns and Grok Build's own `/settings` writes to, which riabuild would then be
rewriting under them on every run. `bypassPermissions` and not `dontAsk`, which reads like
the same thing and silently *denies* everything not pre-approved. Like Codex's `--yolo` it
is a **default rather than an imposition**: Grok Build rejects `--permission-mode` twice
in both the spaced and `=` spellings, so the launcher scans its own arguments and stands
aside wherever the developer named a policy. It does *not* stand aside for
`--always-approve`/`--yolo`, which Grok Build accepts happily alongside the flag and which
means the same thing. And the flag goes **ahead of** `"$@"`, because it is a root option
only — after a subcommand it is `unexpected argument`, so `grok mcp list` has to become
`grok --permission-mode … mcp list`.

A managed-policy pin (`~/.grok/managed_config.toml`, `/etc/grok/managed_config.toml`)
force-disables the bypass regardless of the flag. riabuild does not fight that and should
not — an enterprise deployment pinning approvals on is a decision made above riabuild's
head. Note too that this is an approval policy and **not a sandbox**: `GROK_SANDBOX`
defaults to `off` and riabuild sets neither it nor `GROK_SANDBOX_AUTO_ALLOW_BASH`.

**Never run `x.ai/cli/install.sh`.** It is a competing provisioner — unverified floating
download, `~/.grok/bin`, symlinks into `~/.local/bin` and `/usr/local/bin`, and a `PATH`
block appended to the developer's rcfile, which is exactly what demotes `~/.riabuild/bin`
and quietly breaks the `claude` launcher and the clipboard shims beside it.
`nothing_runs_xais_install_script` is the gate.

All of the above is undocumented, read out of Grok Build 1.0.5 — the shipped binary and the
Apache-2.0 source at `xai-org/grok-build` — which is why the `#[ignore]`d smoke tests in
`shims::grok` run a **real Grok Build** with the arguments `shims::grok::handoff` decides,
for the reason the Codex pair above gives. Run `cargo test -- --ignored` when the pin
moves. Design:
`../docs/superpowers/specs/2026-08-21-grok-build-design.md`.

## The agents window

`riabuild agents` runs Claude Code, Codex and Grok Build sessions in one terminal
window. `~/.riabuild/bin/agents` is a generated shim that execs it, which is the whole of
how the feature has its own executable name — a second binary would mean a `[[bin]]`, an
install line in the Homebrew formula, the deb and the rpm, and another artefact for
`release.yml` to build, sign and strip.

All three open, always. There is no flag to choose: riabuild installs all three, and a
session that has not been spoken to has started no process, so two idle panes cost three
lines on screen.

**The harnesses are driven headless, not embedded.** Each runs in its own structured
output mode and riabuild draws the result; nothing renders a vendor's own full-screen
interface in a pane. That is the choice the whole design rests on, and what it buys is
*state*: screen-scraping three alternate-screen TUIs tells you which pixels changed,
reading their event streams tells you which agent is blocked, what it is running and what
it has spent.

**A turn outlives the window that started it, and nothing here owns a running agent.**
`riabuild agents` starts `riabuild internal agent-turn` *detached* and then only reads
files. The wrapper holds the session's lock, appends the harness's stdout to the session's
spool, and writes down the thread id — three things a third-party binary cannot be asked
to do for itself. So closing the window interrupts nothing, reopening it replays
everything that happened in between, and a reboot loses the process and never the
conversation.

Detaching means three things together, and leaving out any one of them looks like it works
until somebody closes a terminal: `setsid`, so a vanishing terminal's `SIGHUP` reaches its
old process group and not the turn; stdio nulled, so the child holds no descriptor on the
tty; and no `kill_on_drop`, which is what every other spawn in `riabuild-runner` sets.
`CommandRunner::spawn_detached` is the only method that does this and returns no handle —
deliberately, because a process expected to outlive this one is not something this one can
honestly claim to be able to wait for or kill.

**Every harness now runs one child per turn, Claude Code included.** It *can* hold a
session open — `--input-format stream-json` reads a user message per turn off stdin and
never closes it — and that is exactly what detaching rules out: nobody is left holding the
write end of a detached child's stdin, so Claude Code reads EOF and exits. So all three are
started per turn and resumed by id, and the difference between them collapses to how you
spell resume. What it costs is process warmth, not context: `--resume` reloads the
conversation from the harness's own store. Verified against 2.1.235 — `claude -p
--output-format stream-json --verbose --permission-mode bypassPermissions --resume <uuid>
"…"` answers inside the session that id names.

**Liveness is a lock, never a pid.** Whether a turn is running is answered by trying to
take `turn.lock`. That is `remote::channel::lease`'s decision for `remote::channel`'s
reasons — a pid in a file is a claim somebody has to check, a marker outlives its process,
and `kill -0` on a recycled pid answers about the wrong process — and here it also gets a
reboot right for free, because the kernel releases an `flock` however the holder died.

**A session is a directory, and the spool is the harness's own bytes.**
`<root>/agents/<id>/` holds `meta.json` (harness, thread id, profile home, checkout,
title), `events.ndjson`, `turn.lock`, a `pending/` queue of prompts, and `errors.log`. The
spool is the raw NDJSON the harness produced, appended across turns, because replaying it
through the same `Reader` that reads a live turn is what makes a reopened session show what
was on screen rather than a reconstruction of it. Under `root()` and not `tools_root()`, so
two developers on one server are invisible to each other.

`errors.log` is the other half of that, and it is not redundant. The spool holds one
vendor's wire format, so a line riabuild wrote there would decode to nothing — and a
detached wrapper has no stderr anybody reads. Without it, a harness that will not start is
a session that sits idle for ever with no explanation.

**The profile is recorded, never recomputed.** riabuild keeps nine sign-ins for each
harness and a session is only resumable under the one that created it, so `meta.json`
carries the `CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `GROK_HOME` it was started with. Recompute
it and a changed primary account points the next turn at a different store, where it finds
no session and quietly begins a new conversation under the same pane. The *binary* is the
opposite and is resolved per turn: a versioned path moves with every upgrade.

**Every session is started with that harness's approvals off, in its own spelling.**
`--permission-mode bypassPermissions` for Claude Code,
`--dangerously-bypass-approvals-and-sandbox` plus `--dangerously-bypass-hook-trust` for
`codex exec`, `--always-approve` for Grok Build. None is interchangeable — `codex exec`
does not accept the `--yolo` the launchers pass — and `dontAsk`, which reads like the same
thing on two of the three, silently *denies* whatever was not pre-approved and presents as
an agent refusing its own tools. There is no approval round-trip anywhere in
`riabuild-harness`, and that absence is what makes one event model possible.

**Claude Code's `--bare` is deliberately not passed.** It is the flag that would suppress
the remaining hook, plugin and MCP discovery, and its own `--help` says why it cannot be
used here: *"Anthropic auth is strictly `ANTHROPIC_API_KEY` or `apiKeyHelper` via
`--settings` (OAuth and keychain are never read)"*. Every Claude account riabuild manages
is an OAuth sign-in, so `--bare` would break all nine. Full prompt-suppression and
subscription auth are mutually exclusive on that harness; riabuild keeps the accounts.

**Decoders degrade, never fail.** All three wire formats are undocumented or explicitly
unstable, so an unrecognised frame produces no events rather than an error, and a line that
is not JSON is dropped — `codex exec` prints `Reading additional input from stdin...` on
stdout before its first frame. Read **stdout only**: Codex writes `tracing` diagnostics to
stderr, and merging the two produces a decoder that dies on the first retry a flaky
connection causes.

What is pinned against a real binary and what is not is recorded at each match arm.
Claude Code 2.1.235's stream-json is captured verbatim; Codex 0.148.0's *envelope* is too,
but only its failure path, because no OpenAI or xAI sign-in existed on the machine this was
written on — so the successful item bodies and every Grok update shape are marked
`INFERRED` and are the first thing to re-read when a pin moves. Design:
`../docs/superpowers/specs/2026-08-24-riabuild-agents-design.md`.

## Colour

Every colour riabuild prints comes from `riabuild-theme`, chosen by **role** — `Ok`, `Busy`,
`Danger`, `Brand`, `Muted` — never by writing an escape code at the call site. A role
renders itself at each rung of a depth ladder (24-bit → 256 → the original sixteen →
nothing), so a terminal that cannot do truecolor still gets something deliberate, and
`NO_COLOR` or a non-tty destination gets no escapes at all.

The palette is Clubria's own, read from clubria.com: `#f74f25` is the logo mark's fill,
with `--pink`, `--orange` and `--green` beside it. `Muted` and `Strong` stay *attributes*
(dim, bold) rather than becoming a fixed grey — a hardcoded grey is invisible on one
terminal theme and muddy on another.

**The types are ratatui's, and the ladder is not.** `Color`, `Style` and `Modifier` are
re-exported from `ratatui-core` rather than defined here, because riabuild now paints two
surfaces — `riabuild-ui` prints lines past a terminal it does not own, `riabuild-agents`
draws whole frames into one it does — and a private `Rgb` would mean converting at that
boundary, which is a second palette by another name. What ratatui does **not** bring is
the reason `riabuild-theme` still exists: it has no notion of terminal capability at all.
Its backends write a `Color::Rgb` out as a 24-bit escape whatever is on the other end, and
it has no `NO_COLOR`. So a style passes through `Theme::style` (a role) or `Theme::lower`
(any colour a widget picked) **before** it reaches a frame, and `Theme::paint` — the SGR
renderer for line-at-a-time output — is riabuild's too, because ratatui has no API that
renders one styled string.

Two consequences worth keeping. `riabuild-theme` depends on `ratatui-core` and never on
`ratatui`: it describes colour and draws nothing, and `riabuild-fetch` and
`riabuild-runner` depend on it. And `Role::legacy` — the sixteen-colour rendering — is a
**chosen table rather than a nearest-match**, because nearest-match gets it wrong in a way
that matters: `--green` (`#3ddc84`) is nearer to `Cyan` than to `Green`, and `--orange`
lands on `Red` beside `Danger`, so "in progress" and "fatal" would become the same colour.
`nearest_match_is_why_a_roles_sixteen_colour_palette_is_chosen_by_hand` pins that.

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
