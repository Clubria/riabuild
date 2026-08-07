# riabuild-cli

Rust binary that provisions a developer's machine and drops them into the Clubria
environment. Distributed via Homebrew tap `clubria/tap`.

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

A riabuild-managed **server** is the one exception: it may hold its own session
token at `<namespace>/session.token`, mode 0600. It has no keyring, the token is
minted for that server alone, it is labelled and listed in the dashboard, and
`riabuild remote forget` revokes it. Laptops are unchanged, and the Infisical
credential is still brokered per use and never written down.

**Paths and keychain stay behind traits.** v1 targets macOS, but `paths.rs` and
`keychain.rs` are the only files that may know that. Linux support should be an addition,
not a rewrite.

**The version comes from the git tag, never from `Cargo.toml`.** riabuild is versioned by
release date (`2026.08.04`), which semver cannot express, so `Cargo.toml` holds a
permanent `0.0.0` placeholder and `cli.rs` reads `RIABUILD_VERSION` injected by the
release workflow. Do not bump the crate version and do not reintroduce
`CARGO_PKG_VERSION` — a binary reporting a version other than the release it shipped in
makes every launch attempt a `brew upgrade` that cannot change anything. Local builds
report `9999.0.0-dev`, above every real date, so working on riabuild never makes riabuild
replace the binary being worked on. See `../docs/releasing.md`.

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
  api/         riabuild-web client     tasks/       trait, registry, DAG runner, one file per task
  shell/       zsh, bash, fish         shims/       ~/.riabuild/bin generation
```

One task per file. When a file passes roughly 300 lines, it is doing too much.

## Shell integration

`bash --rcfile` **replaces** the user's `.bashrc`; zsh has no `--rcfile` and needs
`ZDOTDIR`. Generated rcfiles must source the user's real config **first**, then apply
riabuild's environment. Getting this wrong silently destroys a developer's prompt,
aliases, and history config, which reads as *riabuild broke my shell*.
