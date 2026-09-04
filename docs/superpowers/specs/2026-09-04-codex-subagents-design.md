# codex-subagents

**Status:** implemented
**Date:** 2026-09-04
**Issue:** [#147](https://github.com/Clubria/riabuild/issues/147), under [#146](https://github.com/Clubria/riabuild/issues/146)

Claude Code can delegate to Codex. `riabuild internal mcp-codex` is a stdio MCP server
compiled into the riabuild binary; Claude Code starts one per session, and its two tools —
`codex` and `codex_reply` — open a Codex session in `riabuild agents`' own store, run one
turn, and return **the last thing Codex said**. The delegated session appears on the rail
as a child of the session that asked for it.

This closes the open question `2026-08-24-riabuild-agents-design.md` left under
"Cross-provider delegation", and by the mechanism it predicted: *an agent in one provider
spawning a subagent in another would be an MCP server that opens a session here*.

## Why a subagent is worth having at all

Not because Codex is better. Because a subagent's *working* — the file reads, the shell
commands, the reasoning — stays out of the context of the agent that asked. That is the
whole economics of Claude Code's own Task tool, and it is the only reason to reach for a
second agent rather than doing the work in the first one.

Which makes the filter the entire design, and everything else plumbing.

## Why not `codex mcp-server`

Codex ships an MCP server of its own, and it was the obvious answer for about an hour. Its
`codex` tool streams the session back. A tool that returns the transcript is a delegation
that costs *more* context than doing the work directly — it is the thing a subagent exists
not to be.

So the filter has to live somewhere, and riabuild already owned every piece it needs:
`riabuild-harness` decodes Codex's NDJSON, `riabuild-agents` runs a turn and writes its
spool. Which means the discarded transcript is not discarded. It goes to a real session in
the store, and the developer reads the whole thing in `riabuild agents` while the calling
agent holds one paragraph. The proxy alternative — sit between Claude and `codex
mcp-server`, forward the handshake, rewrite the result — writes the same JSON-RPC framing
and buys a schema OpenAI documents as changing without notice.

`riabuild-mcp` implements four methods: `initialize`, `tools/list`, `tools/call`, `ping`.
That is the whole surface a stdio server needs, and it is why there is no MCP crate in the
dependency graph — `riabuild-harness` next door has exactly one dependency for the same
reason.

## Which session asked, and how a child knows its parent

**MCP has no field for it.** `initialize` carries the client's name and version and says
nothing about which conversation it is serving. A server cannot ask.

What it can do is read its own environment. Claude Code passes its **whole environment** to
the stdio MCP servers it spawns — verified against 2.1.260 by registering a shell script as
a local-scope server and dumping the environment it was started with. So `turn.rs` sets
`RIABUILD_AGENT_SESSION` to the session id on every turn of every harness, and the server
reads its parent out of `std::env`. Two lines, no protocol support, and authoritative:
riabuild's own string, end to end.

Claude Code also exports `CLAUDE_CODE_SESSION_ID`, which is the same uuid its stream-json
carries as `session_id` and therefore the same value riabuild records as `Record.thread`.
It is a real fallback for a session riabuild did not start, and it is **not used**: a
`~/.riabuild/bin/claude` in a terminal has no store record for that uuid to match, so there
would be no parent to attach to either way. It is written down here because it is the thing
somebody will reach for when this needs extending, and because it is undocumented — read
out of one release, and worth re-checking rather than trusting.

A Claude Code riabuild did not start delegates fine. The Codex session it opens simply has
no parent, and the rail draws it at the top level.

**The variable is set for every harness, not for Claude alone.** It names *this* session,
so a Codex turn that is itself a subagent overwrites what it inherited rather than passing
its own parent's id further down. Without that, the day a second harness is given this
server is the day a grandchild attaches to its grandparent.

## One level, deliberately

Only Claude Code gets the MCP entry, so a Codex session made here has no way to delegate in
turn — depth is 1 by construction rather than by rule. The rule exists anyway:
`Delegate::within_depth` refuses a parent that already has a parent.

It cannot fire today, and that is the point of writing it. The day it *can* fire is a day
somebody added the entry to a second harness, and the failure then is silent rather than
loud — the rail draws one level, so a grandchild would be listed as a sibling of its own
parent, and nothing on screen would say which session had actually asked for it.

## What the rail does with a child

Three signals, in the one channel that was free.

- **Indent.** The same `↳` glyph `transcript_lines` already indents a subagent's *work*
  with, so the rail and the transcript make the same claim the same way.
- **A dimmed state mark**, but only while the child is **idle**. An idle parent's mark is
  green; an idle child's is `Role::Muted`, which is a real colour difference that recedes.
  A child that is *working* or has *failed* keeps the hue every other row uses for that,
  because a failed subagent the developer cannot see is the same bug as a failed session
  they cannot see.
- **A right-aligned `(subagent)` tag**, dropped rather than truncated where the rail is
  narrow — `rail_width` goes down to twenty-two columns, and half of "(subagent)" beside a
  title clipped to nothing identifies neither. The indent survives as the signal.

No new `Role`. The rail's hues are the *state* channel, and a subagent hue would compete
with busy, idle and failed for the same pixels.

**Ordering is what makes "child" true.** `store::arrange` groups each root with its
children, and `drive::restore` applies it *before* any pane is added — because
`App::cursor` indexes `App::panes` directly, so sorting at draw time would leave the cursor
selecting whichever session had been at that row.

A child whose parent is not in the listing is a **root**. Parents are pruned on age like
anything else, and losing a conversation because the session that started it aged out is a
worse outcome than an indent that turns out flat.

## The window notices a subagent that arrives while it is open

Without this the feature would be invisible in the case it matters: the developer watches
Claude sit on a tool call for two minutes, and the subagent appears the next time the
window is opened, which is exactly when its output has stopped being interesting.

`drive::adopt` re-lists the checkout's sessions every twenty-five ticks — about three
seconds — and hydrates any it does not have. `follow` picks the new pane up on the very
next tick, so only its *appearance* is delayed. That cadence is the compromise: `follow`
reads two files per pane and is cheap enough for every tick, and a `read_dir` plus a small
read per session is not.

Two details it would be easy to get wrong. `restore` deletes an empty session it finds —
nobody spoke to it, so there is no conversation to lose — and `adopt` must **not**, because
it meets that exact shape a few milliseconds after the server made the directory and before
the first turn has written a byte. And every adoption re-groups, which moves rows, so the
cursor is restored by **id**; an offer-selected cursor is moved down by the number of panes
that arrived, because the rail puts offers after sessions.

## Where the entry comes from, and where it may never come from

`claude_codex_mcp` writes it into each account's `.claude.json`, at
`projects.<checkout>.mcpServers.codex` — local scope, which is per project. That is the
same object `claude_trust` already creates empty in `new_project_entry`, and the same one
`claude mcp add -s local` writes; the shape was confirmed against Claude Code 2.1.260 by
registering one and reading the file back.

A user-scope server would follow the developer into every repository on the machine,
including ones that are not Clubria's. A `.mcp.json` in the checkout would need approval
before it loaded, and that approval is the dialog riabuild exists to have already answered.

**riabuild-web may never supply this.** `mcpServers` is on
`org_settings::vetting::EXECUTES_A_PROGRAM` — refused loudly rather than stripped — because
an entry there is a command and an argv Claude Code spawns at session start. So the only
two legal sources for an MCP server are the checkout, which arrives through a pull request
and is what `claude_plugins` reads, and this binary. The entry written here names riabuild
itself by the absolute path riabuild is running from, and carries no argument any server
chose.

**It repairs its own path.** That command is `/…/riabuild/<version>/riabuild`, which moves
with every upgrade, so `check()` compares the recorded command and argv against what this
riabuild would write. An entry left by last month's release is drift to repair, not a state
to tolerate — the same reasoning the generated launchers are written under.

**It stands aside from an entry it did not write.** A `codex` server whose args do not
begin `internal mcp-codex` is the developer's own and is left alone. What that does not
provide is an off switch: deleting riabuild's entry brings it back on the next run. The
remedy for a developer who does not want a Codex subagent is not to call the tool, and if
that turns out to be the wrong answer, an opt-out belongs in riabuild's own config rather
than in an invented settings key.

## Sign-in is the sharpest edge

riabuild installs Codex and **signs nobody in to it** — nine empty `CODEX_HOME`s from the
first run. So the first delegation on a fresh laptop is OpenAI's 401, which reaches the
calling agent as an opaque tool error, gets retried, and tells the developer nothing about
the one thing that would fix it.

`Delegate::signed_in` checks for `auth.json` in the profile — or a non-empty
`OPENAI_API_KEY`, which Codex reads in its place — *before* a turn, and refuses with the
launcher name to type: `Run `codex login` in a terminal, then ask again.`

## Costs, said out loud

- **A tool call blocks the caller.** A `codex exec` turn takes minutes and the calling agent
  waits. This matches how the Task tool already feels, and there is no timeout here: killing
  a turn to satisfy one would throw away work the developer can see running.
- **One request at a time.** The loop reads a line, answers it, reads the next. A second
  `tools/call` waits. Turns are serialised per session by the store's lock anyway, and a
  concurrent server would need a task per request, a shared writer, and an interleaving
  nobody can reproduce from a bug report.
- **Attribution stops at the session.** The child names its parent session; it cannot name
  the *tool call* that asked, because MCP carries no such id. `Event::Delegated` remains
  what it always was — Claude's own subagents, at entry level inside one pane — and the two
  mechanisms do not interact.
- **stdout is the wire.** Nothing on this path may print. One line of `riabuild-ui` on that
  pipe is a parse error in Claude Code with nothing to say where it came from, which is why
  `mcp-codex` returns before `connect`.
