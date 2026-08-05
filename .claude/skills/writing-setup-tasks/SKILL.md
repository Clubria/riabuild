---
name: writing-setup-tasks
description: Use when adding, changing, debugging, or reviewing a riabuild-cli setup task, or when a task re-runs unnecessarily, reports success on a broken machine, or fails only on a second run
---

# Writing riabuild setup tasks

Every change to what riabuild does to a developer's machine is a change to a setup task.
Tasks run unattended on machines you cannot inspect, so a task that lies about its state
is worse than no task at all.

## The contract

```rust
pub trait Task: Send + Sync {
    fn id(&self) -> TaskId;
    fn title(&self) -> &str;
    fn version(&self) -> u32;
    fn depends_on(&self) -> &[TaskId];
    fn check(&self, ctx: &Ctx) -> Result<Status>;   // Satisfied | Needs(Reason)
    fn apply(&self, ctx: &mut Ctx) -> Result<()>;
}
```

The runner decides a task needs to run when: there is no record in `state.json`
(`NeverRun`), the recorded version differs from `version()` (`VersionChanged`), a
dependency applied this session (`UpstreamChanged`), or `check()` says so
(`CheckFailed`).

**After `apply()`, the runner re-runs `check()`.** A still-failing check is a hard error
surfaced to the developer, never a recorded success.

## Rules

| Rule | Why |
|---|---|
| `apply()` must be safe to run twice | Tasks re-run on dependency change, version bump, or check failure. There is no "already done" branch to rely on. |
| `check()` must detect real drift, not just first-run absence | A check that only asks "does the file exist?" will report a satisfied machine with an expired token in it. |
| All subprocesses go through `CommandRunner` | It is the only thing that makes `check()` unit-testable. No `std::process::Command` outside `runner.rs`. |
| Declare every dependency in `depends_on()` | This is what makes `UpstreamChanged` meaningful. An undeclared edge means the task silently runs against stale state. |
| Never write a secret to `~/.riabuild/` | Session tokens go to the Keychain. Infisical tokens are short-lived and piped straight into `infisical export`. |
| Failure messages name a next action | "Attempted X, ran `cmd`, stderr was Y, do Z, safe to re-run." |

## When to bump `version()`

Bump it when the *desired end state* changes and `check()` cannot see the difference —
for example, the pinned Node version changes, or a generated rcfile gains a new line.

Do **not** bump it to force a re-run because `check()` failed to notice something broken.
That is a bug in the check. Fix the check.

`check()` is authoritative. `version()` is an escape hatch for drift that is genuinely
unobservable, and nothing else.

## Writing a check that works

A good check answers "is this machine correct right now?", not "did I run before?"

```rust
// Weak: passes on an expired token, a wrong version, a revoked session.
if ctx.paths.env_local().exists() { return Ok(Status::Satisfied); }

// Real: every way this can be wrong is a way this check can fail.
let f = ctx.paths.env_local();
if !f.exists() { return Ok(Status::Needs(Reason::CheckFailed("missing".into()))); }
if !dotenv_parses(&f)? { return Ok(Status::Needs(Reason::CheckFailed("unparseable".into()))); }
if mtime(&f)? < ctx.org.secrets_updated_at { return Ok(Status::Needs(Reason::CheckFailed("stale".into()))); }
if !gitignored(&ctx, &f)? { return Ok(Status::Needs(Reason::CheckFailed("not gitignored".into()))); }
Ok(Status::Satisfied)
```

Ask of each check: what is every way this can be wrong on a machine that ran this task
six weeks ago? Expired tokens, upgraded CLIs, downgraded CLIs, edited files, deleted
directories, revoked access, changed pins.

## Adding a task

1. One file in `src/tasks/`, registered in `tasks/mod.rs`.
2. Declare `depends_on()`. The acyclicity test will catch a cycle; nothing will catch a
   missing edge but you.
3. Write `check()` first, against the drift list above.
4. Write `apply()` idempotently.
5. Unit-test `check()` against a fixture `~/.riabuild` tree and injected `CommandRunner`
   output — satisfied, each failure mode, and the post-apply state.

## Common mistakes

**Reporting success without verifying.** Trusting that `apply()` worked because it did not
return an error. The runner re-checks for you; do not defeat it by writing an `apply()`
that swallows failures.

**Actions that can fail at startup.** `git pull` on a dirty tree, on a detached HEAD, or
mid-conflict fails loudly at the worst moment. Prefer reporting drift and letting the
developer decide. `repo_status` is deliberately report-only for this reason.

**Existence checks standing in for health checks.** The most common cause of "riabuild
said everything was fine but nothing works."

**Testing a proxy instead of the capability.** `gh auth status` listing `read:org` is not
the same fact as "this token can read org membership": GitHub accepts five different
scopes there and folds `read:org` into `admin:org`. So the check rejected machines that
worked, and the repair it prescribed — `gh auth refresh -s read:org` — could never make
the string appear, leaving `riabuild` telling the developer to try again, forever. When a
single API call answers the question directly, that call **is** the check. A check that
cannot be satisfied by the `apply()` that follows it is worse than no check.

**Reaching for the shell.** If a task needs a login shell to work, the design is wrong —
that is why riabuild owns the Node tarball instead of driving nvm.
