# `riabuild agents` — UI and UX

What the window looks like and how it behaves, as the code in this crate implements it.
The *why* behind most of it is in `riabuild-cli/AGENTS.md` ("The agents window") and in
`docs/superpowers/specs/2026-08-24-riabuild-agents-design.md` and
`2026-09-02-agents-window-redesign-design.md`. If this file and the code disagree, the
code is right and this file is out of date.

## In one paragraph

`riabuild agents` is a full-screen terminal window for talking to Claude Code, Codex and
Grok Build side by side, in the current Clubria checkout. The left column (the **rail**)
lists sessions and the signed-in sign-ins a new session can be started under. The right
column (the **pane**) shows the selected row's conversation and a box to type into. Every
row keeps its own half-written prompt. Pressing Enter sends it. The agent works in the
background and never asks for approval. Closing the window stops nothing: agents keep
working, and reopening the window shows everything that happened in the meantime.

## Words

| Word | Meaning |
|---|---|
| **sign-in** | One of a harness's profiles, named as its launcher is: `claude-1` … `claude-N`, `codex-1` … `codex-9`, `grok-1` … `grok-9`. |
| **session** | One conversation with one harness under one sign-in, in one checkout. It exists on disk, has a title, and is listed under SESSIONS. |
| **offer** | A signed-in sign-in on the rail that a new session *could* be started under. Not a session: nothing exists on disk and nothing is counted. Listed under NEW SESSION. |
| **turn** | One prompt and the harness's answer to it. It runs detached from the window. |
| **subagent** | A session that another session started (a Claude Code session handing work to Codex). Drawn under its parent. |
| **draft** | The text in a row's box that has not been sent yet. Each row has its own. |

A session exists only once it has been sent a prompt. Browsing the rail never creates
anything.

## Signing in happens outside the window

Signing into a provider is done by the developer, **outside** `riabuild agents`, with
that harness's own command in an ordinary terminal: `claude-N auth login`,
`codex-N login`, `grok-N login` (or `riabuild claude new` for another Claude account).
The window never signs anything in, never opens a browser, and never offers a sign-in
that is not signed in. It only looks.

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
- On launch it finds out which sign-ins are signed in: every profile of every harness,
  `claude-1` … `claude-N`, `codex-1` … `codex-9`, `grok-1` … `grok-9`. This is local and
  starts no session:
  - **Claude Code**: `claude auth status --json` under that account's config directory,
    all accounts at once. Signed in means it answered `loggedIn` true. It also gives the
    email.
  - **Codex** and **Grok Build**: the profile's `auth.json` exists
    (`~/.riabuild/codex/<n>/auth.json`, `~/.riabuild/grok/<n>/auth.json`). No email.
  - A probe that cannot answer (a `claude` that will not start) counts as not signed in.

  The window draws without waiting for this. The answer arrives once, for all sign-ins
  together, usually within half a second, and fills in NEW SESSION.
- With `--prompt TEXT`, the window waits for that answer first. The prompt then goes to
  the first signed-in sign-in of each harness that has one, before the first frame. That
  creates one session per such harness, at most three.
- The window opens with the **rail** focused and the cursor on the first row: the newest
  session, or the first offer if there are no sessions.
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
   ● ↳ codex-1 write a      ┊  I found the race: the mock resolves first.
               test         ┊  ◌ Bash  pnpm test login
                            ┊
  NEW SESSION               ┊  ⠹ running Bash  pnpm test login
   + claude-1 · ada@club…   ┊
   + codex-1                ┊  › ▏
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
6. **Footer**: key hints for the focused area, or a notice (see "Notices"), followed by
   the quit confirmation while one is pending (see "Quit").

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

**NEW SESSION.** Exactly the sign-ins found signed in on launch, one row each, in the
order Claude Code, Codex, Grok Build and by number within each. A sign-in that is not
signed in is not shown anywhere. Each row: cursor, `+`, the sign-in, and ` · email` when
riabuild knows the sign-in's address and it fits. It is left out rather than cut. Emails
are only ever known for Claude sign-ins.

Before the answer arrives, NEW SESSION reads `checking sign-ins…`. If nothing is signed
in, it reads, wrapped to the rail:

```
Nothing is signed in. Sign in outside riabuild agents with `claude-1 auth login`,
`codex-1 login`, `grok-1 login`, then open it again.
```

Commands are never broken across rows. The list is not refreshed while the window is
open: a sign-in made meanwhile shows up the next time the window is opened.

### The pane

Drawn on a slightly raised background, with a two-column margin inside. Top to bottom:

1. A blank row.
2. **Status line**: the sign-in in bold, ` · email` if known, and on the right the
   session's **total** tokens, `<input> in / <output> out`, once it has reported any.
   The total is every turn added together, cache reads included.
3. A blank row.
4. **Conversation** for a session, or the **splash** for an offer.
5. While a turn is running: a blank row, then the **activity line** (see "What a turn
   is doing"). When no turn is running, neither row is there.
6. A blank row.
7. **Compose box**.
8. A blank row.

**Conversation.** One entry per event, oldest at the top:

| Entry | Look |
|---|---|
| your prompt | `› text`, in the brand colour |
| the agent's reply | plain text |
| the agent's reasoning | muted text |
| a tool call | `◌` running / `✓` succeeded / `✗` failed, then the tool name in bold, then a muted detail (a path, a command) |
| a failure | red text |
| a subagent's work inside its parent | the same, indented with `  ↳ ` |

Long lines wrap. A session with no entries yet shows an empty conversation, with no
placeholder text. The view stays pinned to the newest line unless you scroll up. Changing
rows, editing the draft or sending a prompt pins it to the bottom again.

**What a turn is doing.** One line between the conversation and the compose box. It
says which of four states the selected session is in, worked out from the turn's event
stream and from whether a turn holds the session's lock:

| State | When | Line |
|---|---|---|
| **launching** | a turn was started and its harness has said nothing yet | `⠹ launching claude session…` (`codex` / `grok` for those) |
| **thinking** | the harness is writing and no tool call is open | `⠹ thinking… · claude opus-5 at high` |
| **running a tool** | a tool call was made and its result has not arrived | `⠹ running Bash  pnpm test login` |
| **stopped** | no turn is running | no line at all |

- The spinner is the rail's, in the working colour. `running` is in the working colour
  too, with the tool name in bold and its detail muted, so a tool call does not look
  like thinking. `thinking…` is plain text and what follows it is muted.
- **Model and effort** are shown only as the harness reports them. A part it did not
  report is left out, never guessed: `thinking… · codex` alone is correct.
  - Claude Code: the model comes from the `init` event (`claude-opus-5[1m]` is shown as
    `opus-5`). The effort is shown when `init` includes one; a `-p` turn on 2.1.280
    does not.
  - Codex: its stream has neither. Both come from the `turn_context` line Codex writes
    into the thread's own rollout under `$CODEX_HOME/sessions/`, when one is there.
  - Grok Build: neither is reported.
- When several tool calls are open, the newest is shown: for a subagent, that is the
  subagent's own tool rather than the call that started it. A call an earlier turn left
  open does not count.
- The line stays put while you scroll the conversation. It is cut with `…` to one row,
  never wrapped.
- **Stopped has no text.** When a turn ends, fails, or is killed, the line and the blank
  row above it go away, and the rail's mark is what remains.
- A prompt sent while a turn is running waits behind it. It shows as launching once the
  turn in front of it ends.

**Splash** (an offer), centred both ways in the pane:

```
create a Claude session
login: claude-1 · ada@clubria.com
```

Only the harness name uses the accent colour.

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

## Focus and keys

The keyboard talks to one of two places. `Ctrl-C` asks to quit from either, and a second
press quits (see "Quit"). Key releases are ignored.

**Rail** (footer: `↑↓ move · → open · q quit`)

| Key | Does |
|---|---|
| `↓` `j` `Tab` / `↑` `k` `Shift-Tab` | next / previous row. Runs through sessions and then offers, and wraps around |
| `→` `Enter` | focus the pane, with the caret at the **end** of that row's draft. Typing works immediately. Does nothing when the rail has no rows (no sessions and nothing signed in) |
| `q` | ask to quit; a second `q` or `Ctrl-C` within 5 seconds quits |

There is no key to add a sign-in or pick one from a list: every signed-in sign-in is
already a row.

**Pane** (footer: `enter send · ^v paste · alt+enter newline · ↑↓ scroll · ← sessions`)

| Key | Does |
|---|---|
| any character | typed into the box at the caret. `q`, `j` and digits are just letters here |
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

The mouse is **not** captured. The window reads no mouse event, so a click does nothing
here and a drag is the terminal's own selection.

## Copying

Select with the mouse and copy the way the terminal always does: `Cmd-C` in iTerm2 and
Terminal.app, `Ctrl-Shift-C` in GNOME Terminal, copy-on-select and middle-click where the
terminal has them. Over `riabuild remote` the selection is made by the laptop's terminal,
so it lands on the laptop's clipboard with nothing else involved. riabuild has no copy key
and no selection of its own.

The trade-off is that the terminal sees cells, not panes:

- A drag across lines also picks up the **rail** beside them, since nothing in between
  stops it. To take the pane alone, use a block selection: `Option`-drag in Terminal.app,
  `Cmd-Option`-drag in iTerm2, `Ctrl`-drag in GNOME Terminal.
- A line the pane **wrapped** copies as two lines, and a line may carry trailing spaces
  from the raised background.
- Only what is on screen can be selected. Scroll the pane with `↑` `↓` `PageUp`
  `PageDown` to bring the rest into view.
- Inside tmux with `mouse on`, tmux takes the drag; hold `Shift` to reach the terminal's.

Capturing the mouse would fix the first two and cost the rest: every terminal's own
selection, copy-on-select and middle-click paste would stop working, and riabuild would
have to reimplement all of it.

## What happens when you…

**Send to an offer.** A session is created under that sign-in and added to the **top** of
SESSIONS with the cursor on it. Its title is the prompt, flattened to one line and cut at
60 characters. Then the turn starts. The offer stays on the rail for next time.

**Send to a session.** `› text` appears at once and the session shows as working at once,
with `launching … session…` under the conversation, before the process has started. Any trouble mark is cleared. The turn continues the same
conversation under the sign-in the session was created with.

**Send while a turn is running.** Allowed. The prompt is queued and runs after the current
turn, in the order sent.

**Stop a running turn.** `Esc` in the pane asks it to stop. The turn ends at its next line of
output, or within moments if the harness has gone quiet, and the conversation says it was
stopped in the accent colour — not as trouble, because nothing went wrong.

**Wait.** The window redraws about eight times a second. New output shows up as it is
written, the spinners turn, and the activity line moves between thinking and running a
tool as the agent works. Sessions the window did not start, meaning subagents, show
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

**Quit** (`Ctrl-C` anywhere, or `q` on the rail) takes **two presses within 5 seconds**.

- The first press quits nothing. It shows `press q again to quit` or
  `press ctrl-c again to quit`, naming the key that was pressed, in the warning colour
  at the end of the footer, after the key hints. It does not type anything.
- Either quit key confirms the other: `q` then `Ctrl-C` quits too.
- Any other key cancels it, and the next quit key asks again. In the pane that includes
  `q`, which is a letter there and is typed.
- After 5 seconds with no second press it expires and the message disappears on its
  own, without a keypress.
- On quitting, the screen is cleared, the terminal title is restored, and the shell comes
  back as it was. Running turns are **not** stopped. Drafts are not kept.

**Reopen.** Every session in this checkout comes back, in the same order, with its full
conversation replayed. Running turns show as working, and failures from earlier are still
shown.

## Notices

One line in the warning colour, in place of the footer:

- `Nothing on the clipboard to paste.`
- the command to install a clipboard tool, on a Linux machine that has none
- the error, if reading the clipboard failed

A notice goes away on the **next keypress**, not after a set time. The window never
closes because of one. A quit key is a keypress too, so it clears a notice before asking
to quit; if the two were ever on screen together, the quit message would follow the
notice rather than replace it.

## Agents never ask

Every turn runs with that harness's approvals turned off, so there is no approve/deny
step anywhere in the window. Claude Code turns also use the team's settings file, the
same one every `claude` launcher uses.

## Look

- **No borders, dividers or popups.** The pane is separated from the rail by its raised
  background alone. On terminals with fewer than 256 colours there is no raised
  background, so a muted vertical line in the gap takes its place. Nothing is ever drawn
  over the body.
- **Colours by role**, from `riabuild-theme`: brand (cursor bar, `+`, header name, your
  prompts, `›`, caret), ok/green (idle, tool succeeded), busy/orange (working, tool
  running, the "working" count, the activity line's spinner and `running`), danger/red
  (trouble, failed tool, failure text, a session that could not be started), warn
  (notices, the quit confirmation), muted (headings, sign-ins, unselected titles,
  secondary text), strong (selected title, repository, tool names, key names). Nothing
  uses a hard-coded colour, and `NO_COLOR` turns colour off.
- **ASCII fallback** where the terminal is not trusted with Unicode:

  | Unicode | ASCII |
  |---|---|
  | `▌` cursor | `>` |
  | `●` idle | `*` |
  | spinner / `◐` working | `~` (no spinner) |
  | `…` on the activity line | `...` |
  | `▲` trouble | `!` |
  | `↳` child | `>` |
  | `◌` `✓` `✗` tool | `.` `+` `!` |

- **Cutting**: titles end in `…` when cut. Footer hints are dropped whole from the
  right, and room for the quit confirmation is set aside before them; if even the
  message alone does not fit, it is cut with `…`. An email that does not fit is left out
  entirely. `(subagent)` is left out before the title is cut to nothing.
