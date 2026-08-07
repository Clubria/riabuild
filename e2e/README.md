# The end-to-end test

The real CLI, provisioning a real machine, against a real backend.

```sh
E2E_GITHUB_TOKEN=<token> e2e/run.sh
```

Runs in CI on every pull request via `.github/workflows/e2e.yml`, on a macOS
runner. Takes five to ten minutes, most of it downloading Node, pnpm, Claude
Code and a Convex backend.

## Why this exists

`ci.yml` proves each side is correct in isolation: the CLI's tasks against
canned `gh`, `git` and `node` output, the Convex functions under `convex-test`.
Neither side can catch the failure that actually strands developers — riabuild-web
renaming a field the Rust client deserialises. Both suites stay green while every
laptop breaks.

So this covers the seams no unit test reaches:

| | |
|---|---|
| the `/api/v1` contract | the Rust client parsing what Convex actually serves |
| idempotency | a second run applies nothing, on a machine rather than in a tempdir |
| drift | deleting `~/.riabuild/bin/pnpm` repairs the toolchain and nothing else |
| the shell handoff | real `zsh` and `bash` resolving `node`, `pnpm` and `claude` |
| `CLAUDE_CONFIG_DIR` | still redirecting Claude Code, which is undocumented and therefore only true while something tests it |
| Claude Code accounts | `riabuild claude list` on a real machine, and account 1's sign-in state as *real* Claude Code reports it |
| the Keychain | `security(1)`, on macOS, storing and deleting the session token |

## What is faked

One thing: `app.infisical.com`, by `infisical-stub.mjs`. Convex, GitHub, the
Node tarball, the `gh` and `infisical` downloads and npm are all the real thing.

Everything between the two calls the stub answers — brokering, the short-lived
token, the environment-not-arguments handoff, writing and git-ignoring
`.env.local` — is riabuild's own code, running unmodified. Using a real Infisical
machine identity instead would put the credential that unlocks every dev secret
into GitHub Actions in order to test code we already own.

The stub returns a loud `501` for any path it does not implement, so an Infisical
CLI change surfaces as *"the stub does not implement GET /api/v5/…"* rather than
as an empty `.env.local` and a passing run. It moved from `/api/v3/secrets/raw`
to `/api/v4/secrets` once already.

## The one step CI cannot finish

`claude auth login` opens a browser and waits for a round trip somebody has to
complete. A runner has nobody, and the spec makes a signed-in account 1 a
*blocking* provisioning requirement — so on CI `riabuild` stops there on purpose,
in a sentence rather than a hang:

```
riabuild stopped: signing you in to Claude Code
  ran claude auth login
  riabuild has no terminal to hand the sign-in to, and will not wait for one
```

The suite expects that and asserts it precisely: the refusal has to name the step
and name one action a person can take, and **any other** provisioning failure is
still fatal. It then asserts everything the run did reach, plus everything the
sign-in does not gate — `riabuild claude list`, `riabuild env`, the shell handoff,
`CLAUDE_CONFIG_DIR`, and that account 1 reads as *logged out* rather than *cannot
tell*. That last one is worth its place: unit tests pin riabuild's parse against
canned JSON, and only a real machine pins the JSON.

Two things genuinely go uncovered, because a run that stops at the last task never
reaches the step after it: the generated launchers in `~/.riabuild/bin`, and the
per-account trust keys. That is the task engine's ordinary fail-fast contract
rather than anything about accounts — a failed `project` task costs the shell too.

A third, the `applied=[]` idempotency invariant, is substituted rather than lost.
Its run log is written after the tasks, so an aborted run produces none; `--check`
completes where a real run cannot and writes the same line, and it must report
exactly `claude_accounts,claude_trust` outstanding and nothing else. Their reason
there is *first run*, not *account 1 is not signed in* — `status_for` answers a
task with no state record without calling `check()` at all — which is why the
assertion is on the set of task ids and not on the sentence.

None of this is remembered anywhere. Seed a signed-in Claude Code config directory
under `~/.riabuild/claude/` before the run — `claude_accounts` adopts a directory
it finds on disk — and provisioning succeeds, `SIGN_IN` becomes `done`, and every
gated assertion runs in place of its substitute.

## Test auth

`E2E_GITHUB_TOKEN` has to belong to a **user** who is an active member of the
org, because riabuild checks membership from both sides:

- the CLI's `github_cli` task runs `gh api /user/memberships/orgs/Clubria`
- riabuild-web re-verifies membership before brokering any secret

Actions' built-in `GITHUB_TOKEN` is an installation token, not a user. Both calls
return 403 no matter how it is scoped, so there is no configuration that avoids
needing a real identity.

Create a **fine-grained PAT**:

- Resource owner: `Clubria` (an org owner has to approve it)
- Repository access: none required — the stand-in repo is public
- Organization permissions → **Members: Read**

```sh
gh secret set E2E_GITHUB_TOKEN
```

Everything else the run needs it makes for itself. There is no
`CONVEX_DEPLOY_KEY` here and no Infisical credential: the backend is an anonymous
local Convex deployment, so CI cannot reach production even by accident.

Without the secret the job skips with a warning rather than failing — pull
requests from forks never receive secrets, and a red check a contributor cannot
fix teaches people to ignore red checks.

## How the session is faked, and how it is not

There is nobody in CI to approve a device-code sign-in, so `run.sh` mints a
token, sends only its **SHA-256** to `devSeed:seedForE2e`, and puts the raw token
in the Keychain. Every request after that authenticates the way a real one does:
hashed, looked up in `cliSessions`, checked for expiry and revocation.

`state.json` starts with a record for `login` — and only `login`. The task engine
treats a missing record as `NeverRun` and applies without calling `check()`
first, so without it every run would print a code and poll for fifteen minutes
however good the session already is. What is skipped is the human approval, which
is un-automatable by construction. What is still exercised is everything the
approval produces.

## Two things `--check` does that it says it does not

`--check` is documented as *"Check everything and report, changing nothing"*. It
does still rewrite `state.json` and the `~/.riabuild/bin` shims, because
`run_all` saves state unconditionally and `main.rs` writes shims before the
dry-run return. Neither is harmful today.

The test therefore asserts the part that would mislead the next run — a dry run
must never record a task as *satisfied* — rather than the literal claim. If the
dry run is ever tightened up, tighten this assertion with it.

## Running it locally

Works on macOS and on Linux, with nothing to stage first — `gh` and `infisical`
are riabuild's to install on both platforms, and fetching them is part of what
this tests. Linux skips the Keychain assertions, standing in riabuild's own
`RIABUILD_TOKEN` escape hatch, so the macOS run is the authoritative one.

| Variable | Effect |
|---|---|
| `E2E_GITHUB_TOKEN` | required — see above |
| `E2E_KEEP=1` | leave the scratch directory, backend and stub up for poking at |
| `RIABUILD_BIN` | test an existing binary instead of running `cargo build` |
| `E2E_REPO_SLUG` | the repository to clone (default `Clubria/riabuild`) |

The run provisions into a scratch `HOME` and deletes it afterwards. Your own
`~/.riabuild`, your checkout and your `riabuild-web/.env.local` are left as they
were.
