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

## Invariants

These are not style preferences. Breaking any of them produces a class of bug that is
expensive to find on someone else's laptop.

**Every external process goes through `CommandRunner`.** No direct `std::process::Command`
outside `runner.rs`. This is what makes `check()` unit-testable against canned `gh`,
`git`, `node`, and `claude` output. Bypassing it means the only way to test a task is to
have a real machine in a real state, and the suite gets abandoned.

**All IO is async.** riabuild runs on a current-thread tokio runtime. Filesystem work goes
through `tokio::fs`, HTTP through `reqwest`, and subprocesses through `tokio::process` —
never `std::fs` or `std::process`. A blocking call on the runtime thread stalls every
other future on it, and the symptom is a provisioner that hangs on someone else's laptop
with no output and no error to send anyone.

The exception is **stdio**. `ui.rs` writes with `println!`/`eprintln!`, and
`run_interactive` hands the terminal to a child process — that is a handoff, not IO
riabuild performs. Async stdout buys nothing for line-at-a-time terminal output.

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

**`apply()` must be safe to run twice.** Tasks re-run whenever a dependency changes, a
version bumps, or a check fails. There is no "already done" branch to rely on.

**`apply()` is always followed by a re-run of `check()`.** If the check still fails, that
is a hard error surfaced to the developer — never a silently recorded success. Half the
value of a provisioner is telling the truth about the machine.

**`check()` is authoritative.** `version()` is only a forced-rerun escape hatch for drift
that `check()` genuinely cannot observe. If you find yourself bumping `version()` to work
around a check that does not detect a real state, fix the check.

**No secrets in `~/.riabuild/`.** The riabuild session token goes in the Keychain via
`keychain.rs`. Infisical tokens are short-lived, brokered per use, and piped straight into
`infisical export` — never written down.

**Paths and keychain stay behind traits.** macOS and Linux are both supported, and
`paths.rs`, `keychain.rs`, `tools.rs`, `download.rs` and `update.rs` are the only files
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
  main.rs      top-level flow          cli.rs       clap definitions
  config.rs    ~/.riabuild + state     paths.rs     path resolution (trait)
  keychain.rs  secret storage (trait)  runner.rs    CommandRunner — all subprocesses
  update.rs    version check, re-exec  ui.rs        output and prompts
  move_project.rs  `move-project`      fs_move.rs   rename, or copy across filesystems
  reset.rs     removes ~/.riabuild
  download.rs  fetching and digests    archive.rs   tar and zip extraction
  tools.rs     the gh and infisical releases riabuild owns
  version.rs   version floors          testing.rs   test helpers
  api/         riabuild-web client     tasks/       trait, registry, DAG runner, one file per task
  shell/       zsh, bash, fish         shims/       ~/.riabuild/bin generation
  accounts/    the Claude Code accounts: registry, status, box, `riabuild claude`
```

## Claude Code accounts

A developer has an ordered list of up to nine Claude Code accounts, each a
`~/.riabuild/claude/<uuid>/` config directory with its own sign-in, and each reached by its
own generated launcher: `claude` runs the primary, `claude-1` … `claude-N` run a named one.
The launchers are the only thing that names a config directory — `CLAUDE_CONFIG_DIR` is
deliberately **not** exported into the environment shell, so a `claude` started outside a
launcher cannot land in an account by accident. `riabuild claude list|new|delete|primary`
manages the list, every environment shell opens with the account box, and the org's Claude
settings and the checkout's trust apply to every account, never just the first.

Design: `../docs/superpowers/specs/2026-08-06-claude-accounts-design.md`.

`download.rs` decides where bytes come from and whether they are the right bytes;
`archive.rs` only ever sees a buffer that already matched a digest. Keep that split — it
is what makes "verified before anything is written" a property of the code rather than a
convention.

One task per file. When a file passes roughly 300 lines, it is doing too much.

## Shell integration

`bash --rcfile` **replaces** the user's `.bashrc`; zsh has no `--rcfile` and needs
`ZDOTDIR`. Generated rcfiles must source the user's real config **first**, then apply
riabuild's environment. Getting this wrong silently destroys a developer's prompt,
aliases, and history config, which reads as *riabuild broke my shell*.
