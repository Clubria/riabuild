# The status line, in Rust — and usage collection without a switch

**Status:** implemented
**Date:** 2026-09-05
**Code:** `riabuild-cli/crates/tasks/src/statusline/`, `riabuild internal statusline` in
`crates/cli/src/internal/mod.rs`, `claude_statusline`,
`org_settings::vetting`

Two changes that turned out to be one. Usage collection was opt-in per Claude account and
nobody had opted in; the thing that would have collected was five hundred lines of
JavaScript. Making collection automatic meant touching the collector, and the collector was
the last generated file in riabuild that still held logic.

## Collection is automatic now

[`2026-08-29-usage-tracking-design.md`](2026-08-29-usage-tracking-design.md) argued the
other way, and the argument is worth restating before it is overturned, because it was not
a bad one:

> `riabuild claude` manages up to nine accounts per developer … "a personal subscription
> and one or more work accounts". Instrumenting all nine ships a person's private usage to
> their employer's dashboard. So collection is **opt-in per account**. … Defaulting to on
> and offering a way out was the alternative and is rejected: the developer who never reads
> the release note is exactly the developer whose personal account it would collect.

What that produced was a dashboard with nothing in it. The developer who never reads the
release note also never runs `riabuild claude track 1`, so the same sentence that protected
a personal account switched the feature off for the whole fleet — including for the work
accounts nobody would have objected to. A measurement that is off everywhere is not a
cautious measurement; it is an absent one, with a settings screen that implies otherwise.

So `riabuild claude track` and `riabuild claude untrack` are gone, `UserConfig::tracked_accounts`
is gone, and the account box says nothing about usage because there is nothing left to say.

**The privacy question does not go away, and the answer is now a fact rather than a
setting.** Only an account under `<root>/claude/<uuid>` is ever spooled — one riabuild
created, numbered, and launches. That is the check in `statusline::usage::spool_target`,
and it is what a developer's own install fails: a `claude` started with
`CLAUDE_CONFIG_DIR=~/.claude`, or none at all, writes nothing anywhere. A personal
subscription signed in through `riabuild claude new` *is* collected now, and that is the
change stated plainly rather than hidden behind a derivation. What is collected is
unchanged and is volume, never content: a cost, some durations, a line count, two
rate-limit percentages. Not the repository, not a prompt, not a path.

**The one-a-minute cadence did not need adding.** The status line already clocked itself:
it stats a marker, and starts `riabuild internal usage-flush` detached when the last
attempt was more than sixty seconds ago. That fires while somebody is using Claude Code —
exactly when there is anything to send — and nothing runs on an idle laptop. There is no
daemon, no launchd job and no systemd timer, which is the property that made the status
line the right collection point in the first place.

## The status line is a subcommand

`~/.riabuild/claude-statusline.js` was `include_str!`'d into the binary, written out
verbatim, and run by `node` on every render. It is now `~/.riabuild/claude-statusline`: one
`exec` into `riabuild internal statusline`, which is the shape every launcher in
`~/.riabuild/bin` already has, for the reasons in
[`2026-08-28-launchers-in-rust-design.md`](2026-08-28-launchers-in-rust-design.md) — and
those reasons apply here more sharply than they did there.

- **There is no type checker.** The same sentence, about the same language.
- **No test ran without a subprocess.** Every assertion about what the bar drew wrote the
  shipped bytes to a tempdir, found `node` on `PATH`, spawned it, and read stdout. Those
  are now ordinary unit tests over a `Session` value.
- **A mistake produced a different working status line.** This is the sharp one. `?.` on
  the wrong side of a `??` draws `undefined%`. A renamed key in Claude Code's own state
  file silently takes the email off the line. And a status line whose command *fails* is
  not an error a developer sees — Claude Code renders it as **no status line at all**,
  which this file has already shipped twice.

One thing got faster rather than merely moving: the render path no longer starts a Node
process. Claude Code debounces status line updates at 300ms and cancels the in-flight
script when a newer one supersedes it, so interpreter startup was being paid on every
render of every session.

### What was kept, because it is what made the JavaScript correct

**Everything is derived from `CLAUDE_CONFIG_DIR`, never from `Paths`.** The status line is
started by Claude Code, and on a server one Unix account holds several developers. The
launcher sets `CLAUDE_CONFIG_DIR = <root>/claude/<uuid>`, so the basename is the account and
the grandparent is *that developer's* root — which is where `config.json` is read for the
launcher number, and where the spool is written. A `Paths` here would answer for whoever
the process happens to belong to, and would be wrong for a colleague on the same box.

**Files are read as files.** `git` and `claude auth status --json` both answer
authoritatively and both cost a subprocess per render. `.git/config`, `config.json` and
`.claude.json` are read directly instead, and `oauthAccount.emailAddress` is a key nothing
promises to keep — deliberately, because the failure it has is bounded: one clause goes
missing and everything else is still drawn.

**Nothing may cost a developer their status line.** An unparseable payload still leaves a
marker, a directory that is not a checkout still leaves a marker, a spool that cannot be
written still leaves the whole line, and the line is printed *before* anything touches a
file.

### Two environment variables went away

`RIABUILD_USAGE_SPOOL` was a path riabuild was passing to itself, and `RIABUILD_SELF` was a
copy of its own `argv[0]` for spawning the flush. The launcher sets neither now:
`current_exe()` answers the second, and the first is derived from the account directory.
The variable that remains is the one Claude Code needs anyway.

## `statusLine` is not a team setting any more

This is the part that reaches the other repository, and it makes the architecture rule
simpler rather than bending it.

The org settings may **name** a program and never **carry** one, and `statusLine` was the
one key allowed to name one. Holding that open needed an equality check on both sides:
riabuild-web refused to store anything but `DEFAULT_STATUS_LINE.command`, and the CLI
refused to write anything but the command `claude_statusline` had installed on *that*
machine. So one string had to be identical in two repositories — a path that is
`~/.riabuild/...` on a laptop and, on a server, the shared account's home rather than the
developer's namespace.

Now the server sends no `statusLine` at all. `vetting` drops one if it arrives — quietly,
because every deployment provisioned before this still sends the old one, and a note about
it would appear on every run of every machine to report a thing no lead did wrong — and
writes riabuild's own in its place. The dashboard refuses to store one, and the settings
screen has no status line row.

**Three things follow, and each is the point:**

- What executes on a laptop is chosen by the binary that installed it. Not by a string in
  a dashboard, and not by an equality check that had to be right in two places.
- There is no rollout order. A new CLI against an old deployment replaces the legacy
  `statusLine`; an old CLI against the new deployment sees a settings file with no
  `statusLine`, which its own vetting has always accepted. Neither breaks.
- A machine holding a cached settings file that names the old JavaScript repairs itself.
  `org_settings::check` compares the cached file against what `vet` would write now, so
  drift `updated_at` cannot see — nothing on the server changed — is still drift.

`org.backfillStatusLine` is deleted with the key it backfilled.

## What is not here

The status line still draws exactly what it drew: the marker, the repository, the account,
and the context-window bar. Nothing about the *display* changed, and this is not a licence
to add to it — every clause on that line is a clause a developer reads on every render.

Codex and Grok Build still publish nothing this can consume. The spool line names its
harness from the first version, so adding one is a producer and not a migration; see the
usage tracking spec.
