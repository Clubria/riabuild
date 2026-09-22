# `riabuild agents` — UI and UX

What the window looks like and how it behaves, as the code in this crate implements it.
The *why* behind most of it is in `riabuild-cli/AGENTS.md` ("The agents window") and in
`docs/superpowers/specs/2026-08-24-riabuild-agents-design.md` and
`2026-09-02-agents-window-redesign-design.md`. If this file and the code disagree, the
code is right and this file is out of date.

## In one paragraph

`riabuild agents` is a full-screen terminal window for talking to Claude Code, Codex and
Grok Build side by side, in the current Clubria checkout. The left column (the **rail**)
lists sessions and the sign-ins a new session can be started under. The right column (the
**pane**) shows the selected row's conversation and a box to type into. Every row keeps
its own half-written prompt. Pressing Enter sends it. The agent works in the background
and never asks for approval. Closing the window stops nothing: agents keep working, and
reopening the window shows everything that happened in the meantime.

## Words

| Word | Meaning |
|---|---|
| **sign-in** | One of a harness's profiles, named as its launcher is: `claude-1` … `claude-N`, `codex-1` … `codex-9`, `grok-1` … `grok-9`. |
| **session** | One conversation with one harness under one sign-in, in one checkout. It exists on disk, has a title, and is listed under SESSIONS. |
| **offer** | A sign-in on the rail that a new session *could* be started under. Not a session: nothing exists on disk and nothing is counted. Listed under NEW SESSION. |
| **turn** | One prompt and the harness's answer to it. It runs detached from the window. |
| **subagent** | A session that another session started (a Claude Code session handing work to Codex). Drawn under its parent. |
| **draft** | The text in a row's box that has not been sent yet. Each row has its own. |

A session exists only once it has been sent a prompt. Browsing the rail or the chooser
never creates anything.

## Starting it

```
riabuild agents [--prompt TEXT]
agents [--prompt TEXT]            # ~/.riabuild/bin/agents runs the same thing
```

- It needs a checkout. On a machine with none, it exits with *"Run `riabuild` first,
  which picks a repository and clones it."*
- It works without network access or a riabuild session. It uses tools and a checkout
  that are already installed and makes no calls to riabuild-web.
- Before the window draws, it drops the least recently used sessions past **50 per
  checkout** and the oldest pasted images past **20**. It also deletes sessions an older
  riabuild created that were never sent anything.
- With `--prompt TEXT`, the same prompt goes to each offer on the rail (the first sign-in
  of each harness) before the first frame. That creates one session per offer, except
  under a sign-in known to be signed out.
- The window opens with the **rail** focused and the cursor on the first row: the newest
  session, or the Claude offer if there are no sessions.
- The terminal's title becomes `riabuild agents — owner/repo` while the window is open,
  and goes back to what it was on exit.

Only sessions created in **this checkout** are listed. The header names the repository so
this is visible.

## Layout

```
                                                                    ← blank row
  riabuild agents  Clubria/payments                  2 sessions · 1 working
                                                                    ← blank row
  SESSIONS                  ┊
  ▌⠹ claude-1  fix the      ┊  claude-1 · ada@clubria.com    12,480 in / 903 out
  ▌            flaky login  ┊
   ● codex-1   add a test   ┊  › fix the flaky login test
               for retries  ┊  ✓ Read  src/login.test.ts
   ● ↳ codex-1 write a      ┊  ◌ Bash  pnpm test login
               test (subagent)  I found the race: the mock resolves first.
                            ┊
  NEW SESSION               ┊  › ▏
   + claude-1 · ada@club…   ┊
   + codex-1                ┊
   + grok-1                 ┊
                                                                    ← blank row
  enter send · ^v paste · alt+enter newline · ↑↓ scroll · ← sessions
```

(`┊` marks where the pane starts. It is not drawn; see "Look".)

Top to bottom:

1. A blank row.
2. **Header.** On the left: `riabuild agents`, then the repository as `owner/repo`, which
   is left out if the checkout has no GitHub remote. On the right: `N session(s)`, plus
   ` · M working` when any turn is running. Offers are never counted.
3. A blank row.
4. **Body**: the rail, a two-column gap, and the pane.
5. A blank row.
6. **Footer**: key hints for the focused area, or a notice (see "Notices").

The whole frame has two columns of margin on each side. The rail is a third of the width,
held between 22 and 40 columns. The pane gets the rest. Everything is redrawn every frame,
so resizing needs nothing.

### The rail

Two groups, in this order. The rail scrolls to keep the cursor in view.

**SESSIONS.** If there are none, it reads `none yet`. Each session is **two lines**,
always. The first line, left to right:

| Part | What |
|---|---|
| cursor | `▌` on both lines of the selected row, blank otherwise |
| state mark | `●` idle (green), a turning spinner `⠋⠙⠹⠸` working (orange), `▲` trouble (red), which a session that is out of usage shows even while its turn is still running |
| child mark | `↳ ` if this is a subagent |
| sign-in | e.g. `claude-2`, padded to a fixed width so titles line up |
| title | the session's first prompt. Bold when selected, muted otherwise |

The title carries on onto the second line, in the same column, broken at a word and cut
with `…` if it still does not fit. A short title leaves the second line blank. Subagent
rows end their second line with a right-aligned `(subagent)`, which is left out when the
rail is too narrow for it and a readable title.

Sessions are in the order they were **created**, newest first, and a session never moves:
sending to it, or a turn finishing in it, does not change its place. A session created
in the window appears at the top, where it will also be when the window is reopened. Each
subagent comes directly after its parent. An idle subagent's mark is muted so it stays in
the background. A subagent that is working or in trouble uses the normal colours.

**NEW SESSION.** One row per offer: cursor, `+`, the sign-in, then a tail after ` · `:

- `signed out`, in the warning colour, when riabuild has been told the sign-in is signed
  out. This is never left out, however narrow the rail.
- otherwise the sign-in's email, when riabuild knows it and it fits. Left out rather
  than cut.

The rail starts with three offers, the first sign-in of Claude Code, Codex and Grok Build.
The chooser adds more.

Sign-in state is only ever learned for Claude sign-ins. It arrives a moment after the
window opens. A sign-in nobody has answered for yet shows nothing, never `signed out`.

### The pane

Drawn on a slightly raised background, with a two-column margin inside. Top to bottom:

1. A blank row.
2. **Status line**: the sign-in in bold, ` · email` if known, and on the right the
   session's **total** tokens, `<input> in / <output> out`, once it has reported any.
   The total is every turn added together, cache reads included.
3. A blank row.
4. **Conversation** for a session, or the **splash** for an offer.
5. A blank row.
6. **Compose box**.
7. A blank row.

**Conversation.** One entry per event, oldest at the top:

| Entry | Look |
|---|---|
| your prompt | `› text`, in the brand colour |
| the agent's reply | plain text |
| the agent's reasoning | muted text |
| a tool call | `◌` running / `✓` succeeded / `✗` failed, then the tool name in bold, then a muted detail (a path, a command) |
| a failure | red text |
| a subagent's work inside its parent | the same, indented with `  ↳ ` |

Long lines wrap. A session with no entries yet reads `waiting for the first reply…`. The
view stays pinned to the newest line unless you scroll up. Changing rows, editing the
draft or sending a prompt pins it to the bottom again.

**Splash** (an offer), centred both ways in the pane:

```
create a Claude session
login: claude-1 · ada@clubria.com
```

Only the harness name uses the accent colour. For a sign-in known to be signed out, a
blank line and ``claude-1 is not signed in — run `claude-1 auth login` in a terminal.``
follow, in the warning colour.

**A session that could not be started** (see "Hit a failure") replaces the splash of the
offer it was started from: the error, every line of it, in red, centred both ways. It
stays until a session is started under that sign-in.

**Compose box.** Starts with `›`; its other lines are indented to match.

- Pane focused: the draft with a `▏` caret. The terminal's own cursor is hidden. It
  wraps at words and grows up to **8 rows**. Past that it scrolls so the caret's row is
  always visible.
- Rail focused, draft empty: `press → to write`, muted.
- Rail focused, draft not empty: the draft, muted, on one line and cut with `…`.

**Every rail row has its own draft.** Moving the cursor, whether from the rail or with
`Tab` inside the pane, puts the current draft away under the row it belongs to and brings
out the draft of the row arrived at. Enter only ever sends the draft of the row it was
written in. Drafts last as long as the window is open.

### The chooser

Pressing `n` on the rail opens a bordered box titled ` new session ` over the body. It lists
every sign-in riabuild keeps: `claude-1` … `claude-N` (as many as exist), `codex-1` …
`codex-9`, `grok-1` … `grok-9`, in that order. Each row shows the sign-in and then
`signed out` (warning colour) if riabuild has been told so, or its email if known, or the
harness name (`Claude Code`, `Codex`, `Grok Build`). The list scrolls to keep the cursor
visible.

- It opens on the sign-in of the row you were on, so "another session like this one" is
  `n`, Enter.
- Enter **offers** the chosen sign-in: it is added to NEW SESSION (or found there if
  already present) and the cursor moves to it on the rail. No session is created. Offers
  added this way last as long as the window is open.
- Esc closes it and changes nothing.

## Focus and keys

The keyboard talks to one of three places. `Ctrl-C` quits from any of them. Key releases
are ignored.

**Rail** (footer: `↑↓ move · → open · n sign-in · q quit`)

| Key | Does |
|---|---|
| `↓` `j` `Tab` / `↑` `k` `Shift-Tab` | next / previous row. Runs through sessions and then offers, and wraps around |
| `→` `Enter` | focus the pane, with the caret at the **end** of that row's draft. Typing works immediately |
| `n` | open the chooser |
| `q` | quit |

**Pane** (footer: `enter send · ^v paste · alt+enter newline · ↑↓ scroll · ← sessions`)

| Key | Does |
|---|---|
| any character | typed into the box at the caret. `q`, `n`, `j` and digits are just letters here |
| `Enter` | send the trimmed draft and clear it. An empty draft sends nothing. Focus **stays** in the pane |
| `Alt-Enter` `Shift-Enter` `Cmd-Enter` `Ctrl-J` | a line break (`Shift-Enter` only where the terminal reports Shift) |
| `←` | move the caret left. At the very start of the draft it goes back to the rail instead, keeping the draft |
| `→` `Home` `End` | move the caret (`Home`/`End`: start/end of the whole draft) |
| `Ctrl-`/`Alt-` `←` `→`, `Alt-B` `Alt-F` | move by a word |
| `Cmd-←` `Cmd-→` | start / end of the line |
| `Backspace` / `Delete` | delete before / under the caret |
| `Ctrl-`/`Alt-Backspace`, `Ctrl-H` | delete the word before the caret |
| `Cmd-Backspace` | delete to the start of the line |
| `↑` `↓` | scroll the conversation one line |
| `PageUp` `PageDown` | scroll ten lines |
| `Tab` / `Shift-Tab` | select the next / previous rail row, with focus left in the pane. The draft stays with the row it was written in |
| `Esc` | while a turn is running, ask it to stop. Otherwise back to the rail, keeping the draft |
| `Ctrl-V` | paste |
| any other `Ctrl-`/`Alt-`/`Cmd-` key | nothing. It does not type a letter |

A footer hint that does not fit is dropped whole, from the right, rather than cut.

**Chooser** (footer: `↑↓ account · enter choose · esc back`)

| Key | Does |
|---|---|
| `↓` `j` / `↑` `k` | next / previous sign-in, wrapping |
| `Enter` | offer it and return to the rail |
| `Esc` | return to the rail |

The mouse is not captured, so the terminal's own text selection and copy work as usual.

## What happens when you…

**Send to an offer.** A session is created under that sign-in and added to the **top** of
SESSIONS with the cursor on it. Its title is the prompt, flattened to one line and cut at
60 characters. Then the turn starts. The offer stays on the rail for next time.

**Send to an offer that is signed out.** Refused: a notice says
``claude-1 is not signed in — run `claude-1 auth login` in a terminal.`` and the draft
stays in the box, so Enter sends it once you have signed in.

**Send to a session.** `› text` appears at once and the session shows as working at once,
before the process has started. Any trouble mark is cleared. The turn continues the same
conversation under the sign-in the session was created with.

**Send while a turn is running.** Allowed. The prompt is queued and runs after the current
turn, in the order sent.

**Stop a running turn.** `Esc` in the pane asks it to stop. The turn ends at its next line of
output, or within moments if the harness has gone quiet, and the conversation says it was
stopped in the accent colour — not as trouble, because nothing went wrong.

**Wait.** The window redraws about eight times a second. New output shows up as it is
written, and the spinner turns. Sessions the window did not start, meaning subagents, show
up within about three seconds, under their parent. The cursor stays on the row it was on.

**Hit a failure.**

- If a turn fails, or its harness will not start on a later turn, the reason appears in
  red in the conversation and the session's mark becomes `▲`. It **stays** `▲` after the
  turn ends and until you send that session another prompt.
- If the harness says the sign-in is not signed in, one more red line names the sign-in
  and the command that fixes it, once.
- If a new session **cannot be started** (its directory cannot be created, or its first
  turn cannot be launched), no session is left behind. The cursor goes back to the offer,
  the prompt goes back into its draft with the caret at the end, and the offer's pane
  shows the error in red in the middle (see "The pane"). Enter tries again.

**Paste (`Ctrl-V`).**

- An **image** on the clipboard (PNG preferred, then TIFF) is saved under riabuild's own
  directory, never in the checkout. Its **path** and a space are inserted at the caret, as
  ordinary editable text. That path is what the agent reads.
- Otherwise **text** is inserted with its line breaks kept. `\r\n` and `\r` become line
  breaks, and leading and trailing whitespace is trimmed.
- An image wins when the clipboard holds both.
- On a server reached with `riabuild remote`, the clipboard read is the laptop's.
- `Cmd-V` / `Ctrl-Shift-V` are the terminal's own paste and never reach this handler.

**Quit** (`Ctrl-C` anywhere, or `q` on the rail). The screen is cleared, the terminal
title is restored, and the shell comes back as it was. Running turns are **not** stopped.
Drafts are not kept.

**Reopen.** Every session in this checkout comes back, in the same order, with its full
conversation replayed. Running turns show as working, and failures from earlier are still
shown.

## Notices

One line in the warning colour, in place of the footer:

- `Nothing on the clipboard to paste.`
- the command to install a clipboard tool, on a Linux machine that has none
- the error, if reading the clipboard failed
- the signed-out sentence, when Enter is refused on a signed-out offer

A notice goes away on the **next keypress**, not after a set time. The window never
closes because of one.

## Agents never ask

Every turn runs with that harness's approvals turned off, so there is no approve/deny
step anywhere in the window. Claude Code turns also use the team's settings file, the
same one every `claude` launcher uses.

## Look

- **No borders or dividers.** The pane is separated from the rail by its raised
  background alone. On terminals with fewer than 256 colours there is no raised
  background, so a muted vertical line in the gap takes its place. The chooser's box is
  the only bordered thing on screen.
- **Colours by role**, from `riabuild-theme`: brand (cursor bar, `+`, header name, your
  prompts, `›`, caret, chooser border), ok/green (idle, tool succeeded), busy/orange
  (working, tool running, the "working" count), danger/red (trouble, failed tool, failure
  text, a session that could not be started), warn (notices, `signed out`), muted
  (headings, sign-ins, unselected titles, secondary text), strong (selected title,
  repository, tool names, key names). Nothing uses a hard-coded colour, and `NO_COLOR`
  turns colour off.
- **ASCII fallback** where the terminal is not trusted with Unicode:

  | Unicode | ASCII |
  |---|---|
  | `▌` cursor | `>` |
  | `●` idle | `*` |
  | spinner / `◐` working | `~` (no spinner) |
  | `▲` trouble | `!` |
  | `↳` child | `>` |
  | `◌` `✓` `✗` tool | `.` `+` `!` |

- **Cutting**: titles end in `…` when cut. An email that does not fit is left out
  entirely. `(subagent)` is left out before the title is cut to nothing.
