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
#[async_trait]
pub trait Task: Send + Sync {
    fn id(&self) -> TaskId;
    fn title(&self) -> &str;
    fn version(&self) -> u32;
    fn depends_on(&self) -> &[TaskId];
    fn interactive(&self) -> bool { false }        // needs the developer
    fn writes(&self) -> &[Resource] { &[] }        // shares something with a sibling
    async fn check(&self, ctx: &Ctx) -> Result<Status>;   // Satisfied | Needs(Reason)
    async fn apply(&self, ctx: &mut Ctx) -> Result<()>;
}
```

**`#[async_trait]` is not decoration, and it goes on your `impl` too.** All IO is async
here, so `check()` and `apply()` are `async fn` — and a trait with an `async fn` in it is
not dyn-compatible on stable, while `registry()` is a `Vec<Box<dyn Task>>`. `async_trait`
rewrites both methods to return a boxed future, which is what lets the engine hold your
task at all. Writing the trait block from memory as two synchronous `fn`s is the mistake
this section exists to stop: it does not compile, and the error it produces names
lifetimes rather than the missing attribute.

The runner decides a task needs to run when: there is no record in `state.json`
(`NeverRun`), the recorded version differs from `version()` (`VersionChanged`), a
dependency applied this session (`UpstreamChanged`), or `check()` says so
(`CheckFailed`).

**After `apply()`, the runner re-runs `check()`.** A still-failing check is a hard error
surfaced to the developer, never a recorded success.

## Your task runs beside its siblings

The engine runs one dependency *wave* at a time and runs the tasks in it **concurrently** —
`gh`, `infisical`, `ngrok` and Grok Build have no edges between them, and a cold run used
to download them one after another. What a developer reads is unchanged: each concurrent
task is handed a `Ctx` whose `Ui` records instead of printing, and the wave is replayed in
registry declaration order once it finishes. So the ladder still scrolls past one task at a
time, in the order `registry()` lists them, whichever download was quickest.

Two declarations are how a task opts out of that, and both are yours to get right — nothing
can infer either.

**`interactive()` — this task needs the developer.** Anything that prints a device code,
hands over a pty, or calls `ctx.ui.ask()`. Such a task is run alone, in its declared
position, against the run's own `Ui`. Get it wrong and there is no crash: `Ui::buffered`
answers `interactive() == false`, so `ask()` returns `None` and your task quietly takes the
nobody-is-here path on a machine where somebody *was* here. The rule is mechanical — **if
`apply()` reaches `ctx.ui.ask()`, `ctx.ui.interactive()`, or a `run_interactive` that waits
on a person, say `true`** — and `the_tasks_that_need_the_developer_say_so` in
`engine::tests` is the list you add yourself to.

**`writes()` — this task shares something with a sibling.** `depends_on()` declares
*ordering*, and until the engine ran a wave concurrently that was the same thing as
exclusion: the sequential loop gave every task the machine to itself, so two tasks writing
one file with no edge between them was invisible and free. It is neither now.

The live case is `claude_config`: `claude_trust`, `claude_onboarding` and
`claude_agents_view` each read-modify-write the same per-account `.claude.json`, and
`claude_plugins` runs a `claude` that writes it too. All four are independent, so all four
land in one wave. Two tasks naming a resource in common never run at the same time.

Do **not** reach for an edge in `depends_on()` to fix a shared file. It writes an ordering
nobody means into the graph, it costs the concurrency of everything downstream, and it says
nothing about the next pair. Ask instead: *if this ran at the same instant as its wave, what
would it be inside that something else is also inside?* A file it read-modify-writes, a
directory it renames into place, a lock it takes. If the answer is nothing, `writes()` stays
empty, which is the common case.

Anything a task changes through `ctx.update_config` or `ctx.update_state` needs **no**
resource: both take the state lock and re-read inside it, so concurrent writers cannot lose
each other's edit. That was true before this engine and is why it needed no change.

## Rules

| Rule | Why |
|---|---|
| `apply()` must be safe to run twice | Tasks re-run on dependency change, version bump, or check failure. There is no "already done" branch to rely on. |
| `check()` must detect real drift, not just first-run absence | A check that only asks "does the file exist?" will report a satisfied machine with an expired token in it. |
| All subprocesses go through `CommandRunner` | It is the only thing that makes `check()` unit-testable. No `std::process::Command` outside `riabuild-runner`, which the crate graph enforces — it is the only crate that names `tokio/process` at all. |
| Declare every dependency in `depends_on()` | This is what makes `UpstreamChanged` meaningful. An undeclared edge means the task silently runs against stale state. |
| Never write a **brokered** secret to `~/.riabuild/` | Infisical tokens are minted per use and piped straight into `infisical export`. Four named exceptions exist and a task is not one of them — see below. |
| Failure messages name a next action | "Attempted X, ran `cmd`, stderr was Y, do Z, safe to re-run." |

### The secrets rule, and the four things it does not cover

The rule a task must not break is about the **brokered** secret: the Infisical credential
is minted per use, piped into `infisical export`, and never written down. That has never
been the whole of it, and reading it as "nothing secret ever lands on disk" is how a task
gets written that quietly re-implements one of the exceptions badly.

Four secrets riabuild does keep, none of them brokered, and each one local to the single
machine that made it:

- **this machine's own riabuild session token** — the Keychain where there is one, and
  `~/.riabuild/session.token` at 0600 where there is not, which includes a managed server
  and a headless Linux box;
- **the cache of a *server's* session on a keyring-less laptop**, at
  `~/.riabuild/remote-sessions/<hash>`, so `riabuild remote` does not mint a fresh 90-day
  session on every run and record it nowhere this laptop can revoke it;
- **a server's SSH password**, under `remote-password:<hash>`, kept because one
  `riabuild remote` run opens around ten SSH connections;
- **an issued SSH key**, which is the one that lands on no filesystem at all — it is held
  in an `ssh-agent` riabuild owns, for the length of one bootstrap.

`keychain::keyring_answers` is the only thing that decides whether this machine has a
keyring. `runner.which("secret-tool")` is **not** an answer to that question —
`libsecret-tools` arrives as a transitive dependency on boxes with no D-Bus session bus
at all — and reintroducing that test is how the bug comes back, looking correct and
passing CI. Read "No secrets in `~/.riabuild/`" in `riabuild-cli/CLAUDE.md` before going
near any of this; it is the authority, and duplicating it here would only give it a
second place to drift.

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
let Some(project) = ctx.project_dir() else { return Ok(Status::needs("no checkout yet")) };
let Some(org) = ctx.org.as_ref() else { return Ok(Status::needs("waiting for sign-in")) };
let file = project.join(".env.dev");

// Weak: passes on an expired token, a wrong version, a revoked session.
if tokio::fs::try_exists(&file).await.unwrap_or(false) { return Ok(Status::Satisfied); }

// Real: every way this can be wrong is a way this check can fail.
if !tokio::fs::try_exists(&file).await.unwrap_or(false) { return Ok(Status::needs(".env.dev is missing")); }
let Ok(text) = tokio::fs::read_to_string(&file).await else { return Ok(Status::needs(".env.dev cannot be read")); };
if !parses_as_dotenv(&text) { return Ok(Status::needs(".env.dev is not a readable env file")); }
if modified_millis(&file).await < org.secrets_updated_at { return Ok(Status::needs("the team rotated secrets after it was written")); }
if !is_ignored(ctx, &project, ".env.dev").await? { return Ok(Status::needs(".env.dev is not ignored by git")); }
Ok(Status::Satisfied)
```

Three things in that shape are load-bearing rather than incidental. Every filesystem call
is `tokio::fs` and awaited. `Status::needs` is the convenience for the common
`Needs(CheckFailed(…))`, and the string it takes is printed to the developer as the reason
the task is about to run, so it is a sentence about their machine rather than a status
code. And the checkout is `ctx.project_dir()`, an `Option` — the repository a run is about
is `Ctx::repo`, chosen by the picker or `--repo`, and neither a path nor a slug is
something a task may go and work out for itself.

Ask of each check: what is every way this can be wrong on a machine that ran this task
six weeks ago? Expired tokens, upgraded CLIs, downgraded CLIs, edited files, deleted
directories, revoked access, changed pins.

## Adding a task

1. One file in `crates/tasks/src/`, registered in `crates/tasks/src/lib.rs`.
2. Declare `depends_on()`. The acyclicity test will catch a cycle; nothing will catch a
   missing edge but you.
3. Declare `interactive()` and `writes()` if either applies — see "Your task runs beside
   its siblings" above. Nothing will catch a missing one of these but you either, and the
   symptom of each is silence rather than a failure.
4. Write `check()` first, against the drift list above.
5. Write `apply()` idempotently.
6. Unit-test `check()` against a fixture `~/.riabuild` tree and injected `CommandRunner`
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
