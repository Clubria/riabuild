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
token, the environment-not-arguments handoff, writing and git-ignoring one
`.env.<environment>` per environment the developer may see — is riabuild's own
code, running unmodified. Using a real Infisical
machine identity instead would put the credential that unlocks every dev secret
into GitHub Actions in order to test code we already own.

The stub returns a loud `501` for any path it does not implement, so an Infisical
CLI change surfaces as *"the stub does not implement GET /api/v5/…"* rather than
as an empty `.env.dev` and a passing run. It moved from `/api/v3/secrets/raw`
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

One thing genuinely goes uncovered, and it is the per-account state: the trust
keys, the completed first-run setup, and the agents view riabuild opens on. All
three write into a `.claude.json` that only exists once an account is signed in,
so a machine with nobody at the keyboard has nothing for them to write to.

The generated launchers in `~/.riabuild/bin` used to be the second, and are not
any more. `provision` wrote `engine::run_all(…)?`, so the first failed task
short-circuited the step that writes them — and the machine most in need of a
`claude-1` was the one that did not get one. `provision::after_the_tasks` now
lands the launchers whatever the tasks did, which is most of what carrying on
past a failure was for: the account box's advice on exactly that machine is `run
claude-1 auth login`, and that has to be a command that exists. So the suite
asserts them on both paths, and `claude` on the environment's `PATH` must be the
launcher rather than the Node tarball's own copy on both paths too. What a failed
task still costs is the shell — that much is unchanged, and a failed `project`
task costs it the same way.

That has one consequence for the `CLAUDE_CONFIG_DIR` assertion, which is
otherwise easy to get backwards. The launcher `export`s its own account's
`CLAUDE_CONFIG_DIR` over whatever the caller set, so it can never be the thing
that answers "does Claude Code still honour the variable" — it answers "does the
launcher still set it". The suite therefore reads the binary the launcher names
and puts the question to *that*, and asserts the launcher's half separately, as
the `CLAUDE_CONFIG_DIR=` line in the generated `claude-1`. Both halves together
are the isolation; either one alone reads as a pass on a machine that has lost it.

The other thing the missing sign-in reaches is the `applied=[]` idempotency
invariant, and that is substituted rather than lost.
Its run log is written after the tasks, so an aborted run produces none; `--check`
completes where a real run cannot and writes the same line, and it must report
exactly `claude_accounts,claude_trust,claude_onboarding,claude_agents_view`
outstanding and nothing else — the latter three each write into a `.claude.json`
that only exists once an account does, so the missing sign-in blocks all of them.
The line is read a field at a time rather than matched whole, and `failed` and
`skipped` have to be empty as well: the engine carries on past a failed task, so
the same line now names what riabuild could not check and what it never got to,
and a short to-do list arrived at by not looking is not the invariant.
`claude_plugins` shares their dependency wave and is deliberately absent: it
answers satisfied for a checkout that declares no plugins. Their reason
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

## `e2e/remote/`: the second suite

A different pair of machines. `run.sh` above provisions *this* machine; the two
scripts in `e2e/remote/` provision a Debian container over a real SSH
connection, which is the shape `riabuild remote` exists for. Both run in
`ci.yml`'s "Remote mode against a container" job rather than in `e2e.yml`.

| | | |
|---|---|---|
| `run.sh` | `riabuild remote` end to end: SSH keys, host-key trust, authorising a key onto a shared account, installing and verifying a published release, then the server's own task DAG driven by this branch's binary | needs `RIABUILD_E2E_GH_TOKEN` and `RIABUILD_SERVER_BIN` |
| `channel.sh` | the clipboard channel end to end: a real `xclip` on a real X display, the real `ssh -T … riabuild channel pump` transport, a real shim on the server | needs `RIABUILD_SERVER_BIN`, no token |

`run.sh` gets one stage further than that. The musl gap is closed — `release.yml`
assembles the checksums file from a spelled-out list of all four targets and
fails the release outright if a tarball is missing, so from v2026.08.10 onward a
Linux server installs for real. Signing the server in is closed too: `stub_web.py`
now implements `do_POST`, minting a delegated session and reproducing the real
endpoint's gates — an authenticated caller, one hop only, a reply shaped exactly
as `ServerSessionReply` deserialises.

Closing that immediately uncovered the next thing, which is the shape this keeps
taking. With the sign-in working, the run reached the task DAG and stopped three
tasks in: the container had no `git`, so `gh auth setup-git` could not run. That
is riabuild refusing a machine it genuinely cannot provision, not a gap in the
harness, and the gate correctly declined to forgive it — so the fix is in
`Dockerfile`, where `git` is now installed. It is the only tool riabuild expects
a server to already have; everything else it downloads, verifies and unpacks
itself, so the next "command not found" in there would mean riabuild had grown a
dependency on the host.

What stops the run now is the size of the stand-in rather than a missing handler.
Past the sign-in the server runs the whole task DAG, and there is no Infisical
stand-in reachable from the container. Making the bottom assertions *pass* is a
piece of work of that size, not a fix, so **they still DO NOT RUN** — the
script's own header is the authority.

## Why `run.sh` is in two acts

`riabuild remote` puts riabuild on a server by downloading a **published
release** and verifying its digest. There is no flag and no environment
variable that points it at a local build, and there must not be one: a server
binary chosen by anything other than a signed release is the server-supplied
task manifest the root `CLAUDE.md` forbids. So in a run of this script the
laptop half is the code under review and the server half is whatever shipped
last.

That is only a curiosity until the job is asked to gate a pull request, at which
point it is the same defect as a test that cannot fail — a server-side
regression the change introduces cannot turn it red, and a server-side bug the
change fixes cannot turn it green. It was found the second way round.
v2026.08.21.1's pnpm is linked against `libatomic.so.1`, which no stock Linux
ships, so the released server stopped on `Node and pnpm (it did not take
effect)` for three rounds running — while the fix for exactly that sat in the
branch the job was supposedly gating.

So the script runs twice against the one container.

**Act one — the laptop, and the install.** `riabuild remote` end to end: SSH,
host-key trust held to a named fingerprint, authorising a fresh key, resolving
the server's home directory, `install::ensure_riabuild` fetching the checksums
file and then the tarball from real GitHub and refusing without a digest,
signing the server in, and lending it the laptop's GitHub sign-in. All of that
is the branch's code, and act one judges it — including a new assertion that
the binary which landed *is* the published release, proved by running it on the
server and reading the version back.

Past that assertion act one stops judging, because past it the server is
executing a released binary. A run that falls over before the install is still
fatal; a run that stops after it is handed to act two rather than forgiven.
`known_gap` is unchanged and no branch was added to it.

**Act two — the server, running the branch.** The musl build named by
`RIABUILD_SERVER_BIN` is copied in, and the three invocations `flow::connect`
composes are made against it over the same SSH, in the same order: `internal
gh-sweep`, `internal seed-github` with the token on stdin, and the
`env 'K=V' … '/abs/riabuild' --no-shell` setup run. Each is riabuild's own
subcommand; act two composes the call, never the behaviour.

Its assertions are the ones a released server cannot satisfy: the run log has
to name a local build (`9999.0.0-dev`, the sentinel `version::VERSION` uses when
no release tag injected one), `toolchain` must not appear among the tasks
riabuild recorded as failed, and pnpm has to answer `-v` through its shim on a
container with no `libatomic.so.1`. Those fail against v2026.08.21.1 and pass
with the fix, which is what makes this job a gate rather than a report.

Act two is **not** a substitute for the gated block at the bottom of the script.
It drives the server for one developer with a prefix it composed itself; that
block is two developers, each arriving through a whole `riabuild remote`, and
the isolation between them. Act two's exit status is not asserted either, for
act one's reason turned around: past the toolchain the DAG wants an Infisical
the container cannot reach, so act two asserts what it can observe and names
what it could not reach.

What did change is that the gate can no longer forgive silently. The old one
matched `BaseHTTPRequestHandler`'s stock 501, which every unimplemented route
returned by accident; there is no stock 501 left, every unhandled route logs one
`stub_web: UNIMPLEMENTED <method> <path>` line, and the gate forgives only on that
evidence plus riabuild's own words. The banner names the routes the run actually
asked for and did not get, instead of reciting a paragraph written months ago.
Everything else — including a hang — fails for real, and only after asserting
against the container that the earlier stages ran: a key pair on the laptop, a
host key pinned, riabuild's key in the container's `authorized_keys`.

`channel.sh` runs to the end. It sidesteps the install by copying a locally
built musl binary in, which is the same target a real install downloads — the
trick `run.sh`'s act two now borrows, so the two scripts differ in what they
point it at rather than in whether they use it. Two properties:

- a PNG and a UTF-8 string put on the laptop's clipboard paste on the server
  byte for byte, and a copy made on the server lands back on the laptop
- the pump is killed mid-session and **only** the clipboard fails: setup
  re-runs and still reaches riabuild-web, the environment shell still opens
  with riabuild's `PATH`, a paste degrades to an empty clipboard rather than
  hanging, and a copy fails loudly rather than losing what was copied
- the pump binds the namespaced socket the environment prefix named, a second
  pump is refused while the first is serving and the first keeps its socket, a
  pump killed with `SIGKILL` leaves a stale socket that the next one replaces,
  and a pump whose laptop stops answering unbinds rather than holding the name

The laptop side runs under `Xvfb` with a real `xclip` rather than a scripted
stand-in, because a stand-in is what `clipboard/linux.rs`'s unit tests already
use — only the real tool can prove riabuild's argv is one xclip accepts and
that a PNG survives X11's atoms in both directions. It costs two packages on
the runner and about a second.

`channel.sh` drives the transport riabuild actually ships: one `ssh -T` whose
remote command is the same `env 'RIABUILD_CHANNEL_SOCKET=…' … riabuild channel
pump` that `remote::flow::connect` composes, with argv built to
`supervisor::ssh_args`' shape. It used to stand up an `ssh -N -R` reverse forward
that the exec-transport design deleted, and passed anyway — sshd still forwarded
the socket to an agent that still served it — so `pump`, `mux`, the keepalive and
the socket rebind had no coverage at all. The one `-R` that remains is a plain
TCP forward giving the container a riabuild-web; it is harness plumbing, not the
channel, and is marked as such at the call site.

What still is not covered end to end: `Agent::serve_pipe` and the argv
`supervisor` builds. The laptop half of the pipe is a named stand-in,
`laptop_pipe.py`, because `serve_pipe` is reachable only through a complete
`riabuild remote` run — there is no command line for it. A hidden
`riabuild channel agent --stdio`, about ten lines in `dispatch.rs`, would let
`channel.sh` drop the stand-in and exercise the real laptop half. Nor does either
script cover a real secrets re-pull.

```sh
RIABUILD_BIN=… RIABUILD_SERVER_BIN=… e2e/remote/run.sh
RIABUILD_BIN=… RIABUILD_SERVER_BIN=… e2e/remote/channel.sh
```

Both scripts take both binaries, and both check them up front and name them
with the exact `cargo` line that builds them. Docker, `ssh`, `Xvfb`, `xclip` and `python3` are checked the same
way, each with a sentence saying what it is for — a missing one otherwise
surfaces mid-run as something that reads like a product bug.

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
