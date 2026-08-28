# ngrok, owned and authenticated

**Date:** 2026-08-18
**Status:** Implemented

## Why

A Clubria developer needs a public URL for a laptop more often than the onboarding path
admits: a Stripe or GitHub webhook that has to reach a local server, a work-in-progress
shown to someone who is not sitting beside them, a service on a riabuild-managed box that
is easier to reach through a tunnel than through the firewall in front of it.

Today that is a decision every developer makes alone — install ngrok however their machine
installs things, make an ngrok account, paste an authtoken into a config file. Three
developers end up with three versions of the binary, three personal accounts, and one of
them ends up with no tunnel at all because Homebrew is not set up yet. That is precisely
the class of decision riabuild exists to remove.

So riabuild owns ngrok the way it already owns `gh` and `infisical`, and the team's
authtoken comes from riabuild-web where a lead can change it once for everybody.

## What is new, and what is not

| | Where it lives | Why |
|---|---|---|
| the ngrok binary | `~/.riabuild/ngrok/<version>/ngrok` | riabuild owns every tool it installs |
| the mirrored artifact | a `Clubria/riabuild` release | upstream publishes neither a pin nor a digest — see §1 |
| its sha256 | a constant in `tools.rs` | verified against a digest riabuild published, not one the artifact's host published |
| the org authtoken | riabuild-web, in Convex | one lead sets it, every developer's CLI reads it |
| the authtoken on a laptop | nowhere | fetched per invocation, held in one process's environment — see §2 |

## 1. Distribution: riabuild republishes ngrok

ngrok is the first tool riabuild owns that cannot satisfy the rule in `../../../CLAUDE.md`
as written. Every other tool is pinned to a version in Rust and verified against a
checksum file its own project publishes. ngrok publishes neither.

Its downloads are served by Equinox from one channel URL:

```
https://bin.equinox.io/c/bNyj1mQVY4c/ngrok-v3-stable-linux-amd64.tgz
```

The version in that path is decorative. Probed on 2026-08-18,
`ngrok-v3.22.1-stable-linux-amd64.tgz` and `ngrok-v9.99.9-stable-linux-amd64.tgz` both
return `200` and both serve the identical 12,104,579 bytes that `ngrok-v3-stable-…`
serves. There is no immutable URL to pin, and there is no published checksum file to
verify against. Whatever Equinox is serving the morning a developer runs `riabuild` is
what they get.

Two things riabuild must not do follow from that. It must not fetch a floating URL
unverified, and it must not take a digest from the server, because a digest riabuild-web
chose selects which bytes execute on a laptop and is the task manifest under another name.

So riabuild mirrors. A maintainer downloads the four platform builds, uploads them to a
dedicated release tag on `Clubria/riabuild`, and records the digests in Rust:

```rust
pub const NGROK_VERSION: &str = "3.39.11";
const NGROK_DIGESTS: [(&str, &str, &str); 4] = [ /* os, arch, sha256 */ ];
```

This is not a new trust model. `Formula/riabuild.rb` already pins a `Clubria/riabuild`
release URL beside an inline `sha256`, and Homebrew already refuses the download when
they disagree. ngrok is now fetched on exactly those terms, and a digest committed to
this repository is stronger evidence than one fetched from the same host as the artifact.

`packaging/ngrok/mirror.sh` does the human half — takes the version to mirror as an
argument, downloads all four builds, prints their sha256s, and uploads them under
`ngrok-v<version>`. It executes exactly one of the four, the build for the host it is
running on, and only in order to *refuse*: if that binary does not report the version it
was asked for, the channel has moved on and nothing is published. Naming the version up
front is the point. The script used to unpack the host's download and read the version
out of it, which is running an unverified binary to decide what to trust — the one act
this whole mirror exists to avoid on a laptop. Its output is what a maintainer pastes
into `tools.rs`, so bumping ngrok stays an ordinary reviewable code change, exactly like
bumping `GH_VERSION`.

**The mirror is a release step, not a build step.** A `tools.rs` that names a version
nobody has mirrored yet is a 404 on every laptop. Publishing the assets comes before
releasing the riabuild that points at them.

### What changes in the fetch crate

`Release.checksum_urls: Vec<String>` becomes `Release.checksum: Checksum`:

```rust
pub enum Checksum {
    /// Fetched from the files the project publishes beside the artifact.
    Published(Vec<String>),
    /// Recorded in this repository, because upstream publishes none.
    Pinned(&'static str),
}
```

`gh` and `infisical` keep their exact behaviour under `Published`. `install()` gains one
branch: `Pinned` skips the checksum fetch and compares the download against the constant.
The mismatch message stays the one already there — it names the asset, the URL, both
digests, and says riabuild refused to install it.

## 2. The authtoken: fetched per invocation, written nowhere

`../../../CLAUDE.md` says secrets are brokered, never stored, and names two local
exceptions. The ngrok authtoken is neither: like an issued SSH key it is a long-lived
secret riabuild-web holds. Unlike an issued SSH key it never lands on a filesystem at all.

`~/.riabuild/bin/ngrok` is a shim. It asks riabuild for the token, puts it in its own
environment, and execs the real binary:

```sh
NGROK_AUTHTOKEN=$("$riabuild" internal ngrok-token 2>/dev/null)
export NGROK_AUTHTOKEN
exec "$ngrok_binary" "$@"
```

The token reaches the process through command substitution and an environment variable,
so it is in no argv — `ps` is world-readable on a shared server and shows every other
developer's command lines — and in no file. `/proc/<pid>/environ` is readable only by the
uid that owns the process.

`riabuild internal ngrok-token` is a hidden subcommand in the same family as `gh-sweep`
and `seed-github`: invoked by a generated shim, never by a person. It authenticates with
the session in the keychain and prints the token on stdout.

> **Superseded, 2026-08-28.** The shim is now `exec '…/riabuild' internal ngrok --binary
> '…/ngrok' -- "$@"`, and the token is fetched by the process that goes on to *become*
> ngrok. Every guarantee above is kept and one is strengthened: the credential is no
> longer read off a pipe into a shell variable, so "print nothing else on stdout" stops
> being a rule `internal ngrok-token` had to keep on the shim's behalf. `internal
> ngrok-token` still exists, because a shim written by an older riabuild stays on disk
> until the next provisioning run rewrites it. See
> `2026-08-28-launchers-in-rust-design.md`.

### Why not export it when the environment shell starts

`shell::ShellLaunch` already carries environment pairs, so riabuild could fetch the token
once and hand it to the shell. That is cheaper and it is the wrong trade:

- **Attribution.** An audit row written at shell launch says somebody opened a terminal.
  One written by the shim says somebody used the org's tunnel credential.
- **Rotation.** A token changed in the dashboard takes effect on the next `ngrok`, not on
  the next shell — and a shell left open for a week is normal.
- **Blast radius.** Every process in that shell inherits its environment, and one of them
  is Claude Code. An org credential in an agent's environment is one prompt away from a
  transcript.

The shim pays for this with one HTTPS round trip before ngrok starts, which is
unnoticeable next to establishing a tunnel.

### What this costs, said plainly

**`ngrok` is authenticated inside the Clubria environment shell and does not exist
outside it.** `~/.riabuild/bin` is on `PATH` there and nowhere else. A developer who wants
ngrok in a plain terminal runs it from the environment shell, or types the path. This is
the direct consequence of never writing the token down, and it is the same bargain the
clipboard shims already make.

**Every tunnel is attributed to one ngrok account.** A shared org authtoken means ngrok's
own dashboard cannot say who opened what. riabuild's `auditLog` is the only attribution,
which is why the fetch writes one. Per-developer ngrok identities would fix it and would
also mean every developer making an ngrok account, which is the decision this feature
exists to delete.

**When the fetch fails, ngrok still runs.** Offline, signed out, or with no token
configured, the shim warns on stderr in riabuild's voice and execs ngrok anyway. Refusing
would break `ngrok --version` and `ngrok help` on a plane in order to protect nothing.

## 3. riabuild-web

`orgConfig` gains two fields:

| field | who reads it |
|---|---|
| `ngrokAuthToken` (optional) | the CLI, through the endpoint below; never a browser |
| `ngrokAuthTokenUpdatedAt` | the dashboard, and every CLI through `/org/config` |

**`GET /api/v1/org/ngrok-token`** is its own route rather than a field on `/org/config`,
because it brokers a credential and the whole ceremony applies: authenticate the session,
require `status === "active"`, re-verify Clubria GitHub org membership, write an
`auditLog` row, return `{ token }`. It answers `404 not_configured` when no lead has set
one, with an action naming the dashboard.

Membership is re-verified here for the same reason `/secrets/token` re-verifies it: a
developer who left the org yesterday must lose the org's tunnel today, without anyone
remembering to edit a Convex row.

**`/api/v1/org/config` gains `ngrokAuthTokenUpdatedAt`** — `0` when unset. It is metadata,
not a secret, on a response every run already fetches, so the CLI can tell a developer
their lead has not set a token yet without brokering anything or writing an audit row for
a question nobody asked. Adding a field is the add-only change the `/api/v1` contract
allows.

In the dashboard the value is write-only. `org.get` returns a hint — the last four
characters and the timestamp — and never the token, mirroring how an issued key is
readable as a fingerprint and not as a secret. Every developer's CLI can fetch the value,
so the mask is hygiene rather than a boundary; it costs nothing and it keeps one rule in
one shape. Setting it is lead-only and writes an audit row.

## 4. The setup task

`crates/tasks/src/ngrok.rs`, `depends_on: []`, shaped like `infisical_cli.rs`:

- **`check()`** — the binary exists at `ctx.ngrok()`, reports a version at or above the
  pinned one, and the shim in `~/.riabuild/bin` is present. Every way a laptop can be
  wrong about *the tool* is a way this fails: never installed, half-written download,
  a version bump in `tools.rs`, a deleted `bin/` entry.
- **`apply()`** — installs the pinned release into `~/.riabuild/ngrok/<version>/` and
  writes the shim. Safe twice: the destination is rewritten, and a version already
  installed is simply reinstalled.

**The task never touches the authtoken.** Three things follow, all of them good.
Provisioning works offline once the binary is there. A team whose lead has not set a token
still provisions green — the gap surfaces from `ngrok` itself, which is the moment it
matters. And `check()` stays honest, because a token fetched only to be discarded would
be an audit row per `riabuild` run.

On a server, the shim calls that server's riabuild with that server's own session, so
remote mode needs no special case.

## 5. Testing

| Layer | What is asserted |
|---|---|
| `fetch` | the asset name and mirrored URL for each of the four platforms; `Pinned` skips the checksum fetch; a digest mismatch refuses to install |
| `tasks` | `check()` satisfied, missing, wrong version, missing shim; `apply()` leaves a satisfied machine; the shim's generated text keeps the token out of argv |
| `api` | the endpoint's 401 / 403 / 404 / 200 shapes |
| convex | unauthenticated, non-member, unconfigured, success-plus-audit-row; the lead-only mutation; the masked view never carries the token |
| dashboard | the settings field through the `riabuild-ui` and `visual-testing` skills |

## 6. What this does not do

No reserved domains, no per-developer subdomains, no tunnel management, no `riabuild
tunnel` command. riabuild puts a working `ngrok` on the machine and gets out of the way.
Anything past that is the platform this tool is deliberately not.
