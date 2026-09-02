# Usage tracking

**Date:** 2026-08-29
**Status:** implemented
**Code:** `riabuild-cli/crates/tasks/assets/claude-statusline.js`,
`riabuild-cli/crates/cli/src/internal/usage.rs`, `riabuild-web/convex/usage.ts`

Clubria's developers run Claude Code on **personal Pro and Max subscriptions**. That one
fact decides this whole design, so it goes first.

Every server-side way of answering "who is using how much" is closed to us. Anthropic's
Usage and Cost API, its Claude Code Analytics API and the Enterprise Analytics API all
require org-admin authority over a Console organisation or a Claude Enterprise org, and
Anthropic states outright that the Admin API is unavailable for individual accounts. There
is an endpoint behind Claude Code's own `/usage` — `GET /api/oauth/usage`, bearing a
subscription OAuth token — and it is documented nowhere, returns percentages rather than
tokens, is rate limited hard enough that Claude Code itself falls back to a cached copy,
and is barred to third-party tools by the Consumer Terms that come attached to the
credential. A tracker built on it would be one Anthropic enforcement action away from
taking every developer's account with it.

So the data has to come from the laptop, out of a surface Anthropic publishes for exactly
this purpose. There are two: the OpenTelemetry export, and the status line. This takes the
status line, and the rest of this document is why that is the smaller of the two and what
it costs.

## Why the status line and not OpenTelemetry

**Because it is already running.** `claude_statusline` installs
`~/.riabuild/claude-statusline.js`, the org settings name it, and every launcher passes
`--settings`. The collection point is deployed on every laptop in the fleet today and has
been since 2026-08-17; what it lacks is not a channel but an interest in the fields it is
already being handed. OpenTelemetry would need an endpoint, an OTLP ingest route, a
credential in Claude Code's environment and a new entry on the `env` allowlist — four new
things against nought.

**Because the org settings only name a program.** `vet_status_line` compares the settings'
`command` against `installed_status_line` by exact equality, and the script itself is
`include_str!`'d and compared byte for byte by `check()`. So the collector ships in a
riabuild release, and *changing what it collects needs no change to the dashboard, to
`orgConfig`, or to the vetting allowlist at all*. That is the architecture rule in
riabuild's own `CLAUDE.md` — the server may name a program and never carry one — paying
out. A key whose value the server chose the contents of would have been the task manifest
again; this is the same channel used the way it was designed.

**Because `rate_limits` exists nowhere else.** The status line payload carries
`rate_limits.five_hour` and `rate_limits.seven_day` — `used_percentage` and `resets_at` —
and it carries them *only* for Pro and Max subscribers, which is precisely this fleet.
OpenTelemetry does not emit them. The Admin APIs cannot see these accounts. On a
subscription nobody pays per token, so consumed rate-limit window is not a nice extra
number: it is the only measure of the thing that actually runs out.

The payload is documented and stable, which the transcript JSONL beside it explicitly is
not. That distinction is why nothing here parses `~/.claude/projects/*.jsonl`.

## What the script does, and the two things it must not do

On every render Claude Code pipes the script a JSON object and reads its stdout. The
script keeps printing the label and the context bar exactly as before, and then appends one
line to a spool.

**It does not open a socket.** Claude Code debounces status line updates at 300ms and
*cancels the in-flight script* when a newer update supersedes it. A network round-trip in
that path makes the bar stall whenever the network is slow, and a provisioning tool that
makes Claude Code look broken has traded the wrong thing for fresher numbers.

**It does not hold a credential.** Reaching `/api/v1` needs this laptop's session token,
and every way of giving the script one is something riabuild already refuses: in Claude
Code's environment it is readable by the model through `env`, which is the stated reason
`ngrok` is a shim rather than an exported variable; in a file it violates "No secrets in
`~/.riabuild/`"; and out of the keychain it would be `riabuild-keychain` reimplemented in
JavaScript, in a file that is not riabuild, running on every render.

So the script writes a line and exits, and `riabuild` — which already holds the token and
already speaks `/api/v1` — does the sending.

### Where the spool goes, and how the script knows

The script lives in `tools_root()`, which on a server is shared by every developer with an
account on the box. The spool must not: a usage sample names one person's session. It goes
under `root()`, the per-developer namespace, for the same reason `agents_dir()` does.

The script is a byte-identical constant on every machine, so it cannot have the path
compiled into it. It derives it instead, from the one environment variable it is certain to
have inherited:

```
CLAUDE_CONFIG_DIR = <root>/claude/<account-uuid>
```

`basename` is the account, `dirname` twice is `root()`. Both halves of what the spool needs
come from a variable Claude Code was launched with, and the same derivation is correct on a
laptop and on a server without either being special-cased. A script that cannot find
`CLAUDE_CONFIG_DIR` writes nothing and still prints its bar.

### How the flush is clocked

The script `stat`s the spool. If the last flush was more than sixty seconds ago it spawns

```
$RIABUILD_BIN internal usage-flush
```

**detached and unawaited**, and returns immediately. Detached because Claude Code kills a
superseded status line script, and a flush that is a child in that process group dies
mid-POST; this is the `spawn_detached` the agents window already uses for
`internal agent-turn`. Unawaited because the render must not wait for it.

This is self-clocking, and that is the point. It fires while somebody is using Claude Code
— which is exactly when there is anything to send — and nothing runs on an idle laptop.
There is no daemon, no launchd job and no systemd timer, and therefore no background
service riabuild has to install, supervise or remove.

`RIABUILD_BIN` is set by the Claude launcher's `handoff`, absolute, alongside the
environment it already sets there. It is a path and not a credential. The script never
looks riabuild up on `PATH`, for the reason
`no_shim_looks_riabuild_up_on_the_path` exists.

## `riabuild internal usage-flush`

Takes the per-developer `flock` **non-blocking**. Three Claude Code windows on one laptop
will notice a stale spool in the same second, and the winner is doing work that makes the
other two unnecessary; a blocking lock would build a queue of processes waiting to send
what has already been sent. This is the rule from
[`2026-08-28-many-windows-one-server-design.md`](2026-08-28-many-windows-one-server-design.md)
— an `flock`, because the kernel releases it however the process ends, and never a pid file.

It is an `internal` subcommand, so `update::applies_to` already excepts it from the
self-update check. Without that, a flush every minute is a version check every minute and
eventually a background download nobody asked for.

**Compaction is what bounds the spool.** Only the newest sample for a session carries any
information — see below — so the flush reduces the file to one line per `session_id` before
sending, and writes the remainder back. A laptop that has been offline for a week has a
spool the size of its session count, not its message count.

**Every failure is silent and keeps the spool.** No riabuild session, no network, a 503, an
expired token: warn nobody, leave the file, try again in a minute. This is `log_run`'s
treatment, and for a stronger reason — this runs unattended beside an interactive Claude
Code session, and a provisioner that prints to a developer's terminal because a dashboard
is down has made a usage tracker into an outage.

## `POST /api/v1/usage`

`guard(version: true, org: true)`, like every other endpoint that carries member data.

### Upsert per session, and never sum

`cost.total_cost_usd` and the session token counters are **cumulative for a session and
reset when `/clear` starts a new one**. The newest sample is therefore the whole truth about
that session, and adding samples together overstates by roughly the number of messages in
it. So the server upserts on `(memberId, accountId, sessionId)` and keeps the larger of
what it holds and what arrived — larger rather than latest, so a sample that overtakes
another in flight cannot walk a total backwards.

This is also what makes the write volume trivial: one document per session per flush rather
than one per render.

### Keyed by member, not by email

The obvious join key is the Claude account's email address, and it is the wrong one three
times over. It is **not in the payload** — the status line JSON has `session_id`, `model`,
`workspace`, `cost`, `context_window` and `rate_limits`, and no account identity at all. It
is **weaker than what the request already proves**: the flush authenticates as the member,
so the server knows who this is from the bearer token, and a client-supplied email is a
client-supplied claim. And it is **personal-subscription identity** — these are accounts
like `someone@gmail.com` — so keying on it means Convex holds a durable map of which
private Anthropic accounts each developer owns, which is a thing to decide out loud rather
than to acquire as a side effect of picking a primary key.

`(memberId, accountId)` it is, where `accountId` is the config-directory uuid riabuild
already assigns. It survives `riabuild claude delete` renumbering, which an account
*number* would not, and it distinguishes one developer's two accounts without naming
either.

### No token counts, which is not an omission

The obvious column is tokens, and the status line cannot supply it.
`context_window.total_input_tokens` reads like a session total and is documented as
something else: "token counts **currently in the context window**, from the most recent API
response". It is `0` before the first response and it *drops* after every `/compact`.
Merged by maximum — which is the rule above, and the right rule for everything else — it
would report the largest the context ever grew. That is a real measurement of a different
subject, and under a column headed "tokens used" it would be wrong on every row.
`current_usage` is worse: it is the last API call alone.

Claude Code publishes no cumulative billed-token figure to a status line. Grok does —
`context_window.session_input_tokens`, explicitly "billed across the whole session,
monotonic" — so the field may arrive when Grok joins, for Grok's rows only. Until then the
cumulative measure of volume is `cost.total_cost_usd`, and storing a number nobody can
populate would be worse than the gap it papers over.

### What a lead sees, and what they do not

Notional cost, sessions, lines changed and rate-limit headroom per member. **Not** which
repository, not a prompt, not a file path — the status line payload carries
`workspace.repo` and this deliberately drops it. A usage tracker that also reports what
each developer was working on is a different product with a different conversation attached
to it.

`total_cost_usd` is rendered as *list-price equivalent* and labelled as such wherever it
appears. On a subscription it is what the session would have cost against the public API
price sheet; it is a reasonable measure of relative effort and it is not money anyone spent.
Left unlabelled it ends up in a budget.

## Personal accounts

`riabuild claude` manages up to nine accounts per developer and
[`2026-08-06-claude-accounts-design.md`](2026-08-06-claude-accounts-design.md) says plainly
what they are for: "a personal subscription and one or more work accounts". Instrumenting
all nine ships a person's private usage to their employer's dashboard.

So collection is **opt-in per account**. `riabuild claude track <n>` marks an account as
work, and only a marked account's samples are ever flushed. An unmarked account's script
writes nothing — not a spool that goes unsent, nothing — and `riabuild claude` shows which
accounts are tracked beside the emails it already lists, so the state is visible in the
place a developer already looks rather than in a document they were told about once.

Defaulting to on and offering a way out was the alternative and is rejected: the developer
who never reads the release note is exactly the developer whose personal account it would
collect.

## Testing

The decisions worth pinning are pure and are tested that way:

- `sample_from_payload` against a recorded status line JSON, including the payload with no
  `rate_limits` (an API-key or Console login) and the one with no `cost`.
- `compact` — that it keeps one line per session, that it keeps the *largest*, and that a
  malformed line is dropped rather than poisoning the file.
- `spool_target` — that `<root>/claude/<uuid>` yields the right root and account on both a
  laptop path and a `~/.riabuild-remote/<member>` one, and that an absent
  `CLAUDE_CONFIG_DIR` yields nothing.
- The script's own behaviour, through `node`, over the recorded payload: that it still
  prints the label and bar, that it appends exactly one line, and that it writes nothing for
  an untracked account.

Convex-side, `usage.test.ts` covers the upsert taking the maximum, the rejection of a
sample naming another member, and a candidate's read returning their own rows only.

## What this is not

It is not per-turn accounting; the status line reports session cumulatives and this stores
them. It is not a spend report; see the labelling rule above. And it does not reach Codex or
Grok yet — Grok publishes a status line of the same shape and can join later, and Codex
publishes no status line callback at all
([openai/codex#17827](https://github.com/openai/codex/issues/17827) is open), so when Codex
arrives it will arrive over a different producer into the same spool format. The spool line
therefore names its harness from the first version, so that adding one is a producer and not
a migration.
