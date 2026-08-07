# End-to-end CI — Design

**Date:** 2026-08-06
**Status:** Implemented
**Scope:** One macOS CI job that runs the real CLI against a real backend.

## The gap this closes

riabuild has two test suites and neither can see the failure that matters most.

`riabuild-cli` tests every `check()` against canned `gh`, `git` and `node`
output — which is exactly what makes them fast and worth having, and exactly why
they cannot notice that the server stopped sending a field. `riabuild-web` tests
its Convex functions under `convex-test`, in a runtime with no Rust in it.

So `riabuild-web` can rename `repoSlug`, both suites stay green, and every
installed CLI fails to deserialise `/api/v1/org/config` on its next launch. The
contract between the two deployables is the least tested thing in the repository
and the most expensive thing to get wrong: an old CLI in the field cannot be
fixed by a deploy.

Everything else this job covers follows from the same principle — test the
things that only exist when the parts are put together on a real machine.

## Shape

```
.github/workflows/e2e.yml   supplies credentials, calls run.sh
e2e/run.sh                  the whole test, runnable on a laptop
e2e/infisical-stub.mjs      stands in for app.infisical.com
e2e/README.md               why, and how to run it
```

The logic is a script rather than inline YAML for one reason: a failing e2e that
can only be reproduced by pushing commits at a runner gets disabled. `e2e/run.sh`
runs on any Mac with one environment variable set.

## macOS, not merely preferably

`security(1)`, the ad-hoc signed binary and the `~/Documents/Clubria` checkout
location are macOS behaviour, and macOS is the platform riabuild ships to. The
script also runs on Linux — minus the Keychain assertions — so the flow can be
debugged without a Mac, but the macOS run is the authoritative one.

## What is real, and the one thing that is not

| Real | Faked |
|---|---|
| Convex backend, all five `/api/v1` endpoints | `app.infisical.com` |
| GitHub org membership, from the CLI *and* from riabuild-web | — |
| Node tarball download and SHASUMS verification, pnpm, Claude Code | — |
| `gh repo clone`, origin verification, `git check-ignore` | — |
| macOS Keychain, generated rcfiles, the shell handoff | — |

Everything between the two calls the stub answers — brokering, the short-lived
token, passing it in the environment rather than the argument list, writing and
git-ignoring `.env.local` — is riabuild's own code running unmodified.

A real Infisical machine identity would mean putting the credential that unlocks
every dev secret into GitHub Actions in order to test code we already own. The
stub returns a loud `501` for any path it does not implement, so the failure mode
of an upstream change is a named unimplemented endpoint rather than an empty
`.env.local` and a passing run. That endpoint has already moved once, from
`/api/v3/secrets/raw` to `/api/v4/secrets`.

## The backend needs no credential

`CONVEX_AGENT_MODE=anonymous` makes the Convex CLI download `convex-local-backend`
and run it against local state, with no Convex account. The deployment writes its
own client and HTTP-action URLs into `.env.local`, which the run reads.

There is therefore no `CONVEX_DEPLOY_KEY` in this workflow, and no configuration
mistake that could point it at production.

## Test auth

One secret: `E2E_GITHUB_TOKEN`, a fine-grained PAT with organisation
**Members: Read**.

It cannot be avoided. riabuild checks org membership from both sides — the CLI
runs `gh api /user/memberships/orgs/Clubria`, and riabuild-web re-verifies before
brokering any secret — and both need a token belonging to a *user*. Actions'
built-in `GITHUB_TOKEN` is an installation token and gets a 403 from both
regardless of how it is scoped.

`RIABUILD_DEV_AUTH` is deliberately **not** set. It would make the dashboard's
membership check return `member` unconditionally. The `/api/v1` re-verification
never consults it, and this run wants the real check on both sides.

The repository is public, so pull requests from forks receive no secrets. The job
warns and skips rather than failing there: a red check a contributor cannot fix
teaches people to ignore red checks.

## The session, and the one place the run puts its thumb on the scale

There is no browser in CI to approve a loopback sign-in. `run.sh` mints a token,
sends only its SHA-256 to `devSeed:seedForE2e`, and puts the raw token in the
Keychain. Every request after that authenticates the way a real one does. A
fixture that inserted a raw token would be testing a system that does not exist.

`state.json` starts with a record for `login`, and only `login`. The task engine
treats a missing record as `NeverRun` and applies without calling `check()`
first, so without it every run opens a browser and times out after three minutes
however good the session already is.

What is skipped is the browser approval, which is un-automatable by construction.
What is still exercised is everything it produces.

## What is asserted

1. `--check` reports work to do, clones nothing, and records no task as
   satisfied.
2. A full run exits 0 and leaves: nine task records, Node and pnpm at the pinned
   versions, a checkout whose `origin` is the repository the *server* named,
   valid `org-settings.json` carrying this deployment's marker, a Claude Code
   account with its launchers in place and no retired `c` launcher beside them,
   and a `.env.local` that parses, carries brokered secrets and is git-ignored.
3. No secret was written anywhere under `~/.riabuild`.
4. The stub saw both the broker call and the CLI's fetch, and was never asked for
   anything it does not implement.
5. A second run logs `applied=[]`.
6. Deleting `~/.riabuild/bin/pnpm` repairs `toolchain`, cascades to
   `claude_accounts`, and leaves `login`, `github_cli` and `project` alone.
7. Real `zsh` and `bash`, spawned through `riabuild shell`, resolve `node`,
   `pnpm` and `claude` inside the environment, and the generated `.zshrc` sources
   the developer's own first.
8. `CLAUDE_CONFIG_DIR` still keeps Claude Code's configuration out of `$HOME`.
9. `riabuild logout` empties the Keychain and the next `--check` still reports.

Assertions are on filesystem state, exit codes, `riabuild env`'s `export` lines
and the run log's `applied=[…]` — never on human-facing output, which is meant to
change.

## Three findings

All three were found by this test. The first is fixed here because it is what
the test caught and because leaving it would mean shipping a red assertion; the
other two are recorded rather than papered over, and belong in changes of their
own.

### Fixed: a signed-out `riabuild --check` failed instead of reporting

`org_settings::check()` compares the cached settings against the server. With a
valid cache on disk and no session it asked anyway, took a 401, and `?` turned
that into a hard error — so `riabuild --check` refused to report on exactly the
machine whose problem was an expired session, which is the moment that command
matters most.

Its sibling tasks already guard for this — `project` and `env_local` both return
`Needs("waiting for sign-in")` when there is no session — so the fix is the same
guard, plus a regression test.

Worth noting how narrow the window is: in a real run `login` applies first and
everything downstream is authenticated, so only `--check` while signed out ever
reached it. No unit test could have found it, because it only exists once the
tasks run in order against a real server.

### Not fixed

**`--check` is not read-only.** It is documented as *"Check everything and
report, changing nothing"*, and it rewrites `state.json` and the
`~/.riabuild/bin` shims — `run_all` saves state unconditionally, and `main.rs`
writes shims before the dry-run return. Harmless today. The test asserts the part
that would mislead the next run: a dry run must never record a task as satisfied.

**`NeverRun` skips `check()`.** A machine that is already correct but has lost
`state.json` re-applies every task rather than verifying them, which for `login`
means an unnecessary browser sign-in. This is rule 1 of the engine as specified,
but it sits awkwardly against *"`check()` is authoritative"* and against
`State::load`'s own comment that unreadable state means *"check everything
again"*. Worth revisiting on its own terms.

## Cost

Five to ten minutes on a macOS runner per pull request, most of it downloading a
Convex backend, a Node tarball, pnpm and Claude Code. Capped at 30 minutes so a
hang is a failure rather than an hour of billing.
