# The developer's own `infisical`, signed in per command

**Date:** 2026-08-27
**Status:** Implemented

## Why

riabuild fills `.env.dev` and `.env.staging` on every run: it asks riabuild-web for a
short-lived Infisical credential, runs `infisical export` with that credential in the
child's environment, writes the files, and forgets the credential. That has worked since
the beginning and is the reason the whole product exists.

Beside it sat an `infisical` on the developer's `PATH` that had never been logged in.
Typing the command riabuild itself runs got:

```
Please either run infisical init to connect to a project or pass in project id with --projectId flag
```

and, with a project id, `error: invalid service token entered`. So a developer who wanted
one secret, or wanted to run their dev server under `infisical run`, or wanted to check
what the team had rotated, had two options and both were wrong. `infisical login` writes a
credential to the machine, which is the one thing riabuild's secrets rule forbids. Copying
a token out of somewhere is worse.

The gap was not a missing feature. It was riabuild owning a tool, putting it first on
`PATH`, and then leaving it unusable for the single thing it is for.

## What is new, and what is not

| | Where it lives | Why |
|---|---|---|
| the infisical binary | `~/.riabuild/infisical/<version>/infisical` | unchanged — riabuild owns every tool it installs |
| `~/.riabuild/bin/infisical` | a hand-back to `riabuild internal infisical` | was an `exec` line; see §1 |
| the credential | one process's environment, for one command | unchanged — brokered per use, written down nowhere |
| which project, environment, folder | riabuild-web's answer, filled in per command | see §2 |
| `infisical login` | never run by riabuild | it stores a credential; that is what this replaces |

Nothing about brokering changes. `POST /api/v1/secrets/token` is the same route
`env_local` has always called, the credential is as short-lived as it was, and each use is
audited. What changes is that the developer can reach it.

## 1. The shim hands the command back to riabuild

ngrok's shim is the precedent — `~/.riabuild/bin/ngrok` fetches the team's authtoken in a
command substitution and execs ngrok with it in the environment — and infisical
deliberately does **not** copy it.

ngrok needs one value. infisical needs five: the token, the API URL, the project, the
environment and the secret path. Reading five values back into a POSIX shell means either
an `eval` of what the server said, or a here-document — which `dash`, and most other
shells, implement by writing a temporary **file**. A brokered credential in `/tmp`, however
briefly, is the thing this whole path exists to prevent.

So the shim is four lines and carries nothing:

```sh
exec "<riabuild>" internal infisical "$@"
```

riabuild brokers, assembles the environment, and starts infisical itself through
`run_interactive` — a handoff, so the developer's terminal, working directory and exit
code are infisical's. The credential exists in two process environments and nowhere else.

Two consequences worth stating. The shim does not name the infisical binary, so "which
infisical" has one answer (`Ctx::infisical`) rather than two that disagree after a version
bump. And riabuild prints **nothing on stdout, ever** — `infisical export > .env` is an
ordinary thing to type, and the child inherits this process's stdout.

## 2. Environment variables are not enough, and finding out cost a silent success

`INFISICAL_TOKEN`, `INFISICAL_API_URL` and `INFISICAL_PROJECT_ID` are read by the CLI and
do the job. `INFISICAL_ENVIRONMENT` and `INFISICAL_SECRET_PATH` exist too, and are
documented as fallbacks — but they are consulted only by the commands whose own flag
carries **no default**. `export`, `run` and `secrets` all default `--env` to `dev` and
`--path` to `/`, and a default counts as an answer, so those two variables are inert
exactly where a developer needs them.

On a team whose secrets live in a folder — which is what `INFISICAL_SECRET_PATH` on the
riabuild-web deployment means, and what `env_local` passes as `--path` on every pull — the
first cut of this change produced an `infisical export` that authenticated perfectly,
exited `0`, and printed nothing. An empty answer that looks like a working command is
worse than the sign-in error it replaced.

So the scope is passed the way `env_local` passes it, as flags, for the three subcommands
that take them:

```
infisical export                 →  infisical export --env=dev --path=/apps
infisical run -- pnpm dev        →  infisical run --env=dev --path=/apps -- pnpm dev
infisical export --env=staging   →  unchanged
```

The shape is the one the Codex and Grok launchers already use for `--yolo`: riabuild
supplies a default and **stands aside wherever the developer expressed one**. `--env`,
`--env=`, `-e` and the three `--path` spellings all count as expressing one, because
appending a second copy resolves to the last for `--env` and to *both* for `run --path`,
which is a `stringArray`. Either way riabuild would be answering a question the developer
had already answered — which, on a shim that is the only `infisical` their `PATH` can
reach, would make working against staging inexpressible.

Nothing is added after a bare `--`. What follows it belongs to the program `infisical run`
is starting, and neither `--path` nor a scan for `--help` may reach it.

The list of scoped subcommands is closed rather than open, and that is the safe direction
here: `--path` on a subcommand that has none is not a flag infisical ignores, it is
`unknown flag` and a command that used to work.

## 3. What is not brokered

A credential is minted per invocation, which is the same trade ngrok makes: a credential a
lead revokes this morning stops working this morning, and the audit row says somebody read
the team's secrets rather than that somebody opened a terminal.

Some invocations are skipped, and the reason is `infisical scan`. It is the subcommand
developers install as a pre-commit hook, so it runs on every commit, and minting a
credential and writing an audit row to scan a diff for leaked secrets answers a question
nobody asked. Beside it are the ones that are about the developer's own machine rather
than the team's project — `login`, `user`, `vault`, `reset`, `help`, `completion` — and
the invocations that print help or a version.

That list is closed in the other direction from §2's: **anything unrecognised is
brokered for**, so a subcommand infisical adds after this was written is signed in rather
than mysteriously signed out.

## 4. Failure is not fatal

A broker that fails — offline, signed out, a dashboard that is down — prints riabuild's
explanation on stderr and runs infisical anyway. `infisical --version` and `infisical
scan` are worth having on a plane, and what the developer meets is infisical's own "you
must be logged in" with a reason above it rather than instead of it.

The one thing that *is* fatal is a missing binary, which means a half-removed
`~/.riabuild` rather than a machine mid-setup, and is reported as such — the alternative
is `No such file or directory` naming a path the developer never chose.

## 5. What it costs

**A credential per invocation.** Fetching one is a round trip to riabuild-web and a mint
against Infisical. Someone running `infisical secrets` in a loop pays for each. The
alternative — one credential exported into the environment shell — was rejected for
ngrok's reason and one more: every process in that shell inherits it, and one of them is
Claude Code.

**Attribution is unchanged and still per developer.** The credential is minted against the
developer's own riabuild session, so `auditLog` records who read what. This adds rows; it
does not blur them.

**Migration is free.** `check()` compares the shim's *text* against what this riabuild
would write, so every machine provisioned before this change sees drift on its next run
and has the file rewritten. The task's `version()` does not move, because that escape
hatch is for drift a check genuinely cannot observe, and this is not that.
