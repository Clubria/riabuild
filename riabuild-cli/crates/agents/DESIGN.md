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
**pane**) shows the selected session's conversation and one line to type into. Typing a
prompt and pressing Enter sends it. The agent works in the background and never asks for
approval. Closing the window stops nothing: agents keep working, and reopening the window
shows everything that happened in the meantime.

## Words

| Word | Meaning |
|---|---|
| **sign-in** | One of a harness's profiles, named as its launcher is: `claude-1` … `claude-9`, `codex-1` … `codex-9`, `grok-1` … `grok-9`. |
| **session** | One conversation with one harness under one sign-in, in one checkout. It exists on disk, has a title, and is listed under SESSIONS. |
| **offer** | A sign-in on the rail that a new session *could* be started under. Not a session: nothing exists on disk and nothing is counted. Listed under NEW SESSION. |
| **turn** | One prompt and the harness's answer to it. It runs detached from the window. |
| **subagent** | A session that another session started (a Claude Code session handing work to Codex). Drawn under its parent. |

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
- Before the window draws, it drops the oldest sessions past **50 per checkout** and the
  oldest pasted images past **20**. It also deletes sessions an older riabuild created
  that were never sent anything.
- With `--prompt TEXT`, the same prompt goes to each of the three offers on the rail
  (the first sign-in of each harness) before the first frame. That creates three sessions.
- The window opens with the **rail** focused and the cursor on the first row: the newest
  session, or the Claude offer if there are no sessions.

Only sessions created in **this checkout** are listed. The header names the repository so
this is visible.

## Layout

```
                                                                    ← blank row
  riabuild agents  Clubria/payments                  2 sessions · 1 working
                                                                    ← blank row
  SESSIONS                  ┊
  ▌⠹ claude-1 fix the flak… ┊  claude-1 · ada@clubria.com    12,480 in / 903 out
   ● codex-1  add a test f… ┊
   ● ↳ codex-1 write a te…  ┊  › fix the flaky login test
                            ┊  ✓ Read  src/login.test.ts
  NEW SESSION               ┊  ◌ Bash  pnpm test login
   + claude-1               ┊  I found the race: the mock resolves first.
   + codex-1                ┊
   + grok-1                 ┊
                            ┊  › ▏
                                                                    ← blank row
  type to write · enter send · ^v paste · ↑↓ scroll · ← sessions
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
held between 22 and 40 columns. The pane gets the rest.

### The rail

Two groups, in this order:

**SESSIONS.** If there are none, it reads `none yet`. Each row, left to right:

| Part | What |
|---|---|
| cursor | `▌` on the selected row, blank otherwise |
| state mark | `●` idle (green), a turning spinner `⠋⠙⠹⠸` working (orange), `▲` trouble (red) |
| child mark | `↳ ` if this is a subagent |
| sign-in | e.g. `claude-2`, padded to a fixed width so titles line up |
| title | the session's first prompt, on one line. Bold when selected, muted otherwise. Cut with `…` when too long |
| `(subagent)` | right-aligned on subagent rows, and left out if the rail is too narrow for it and a readable title |

Sessions are ordered by most recent activity, newest first. Each subagent comes directly
after its parent. An idle subagent's mark is muted so it stays in the background. A
subagent that is working or in trouble uses the normal colours.

**NEW SESSION.** One row per offer: cursor, `+`, the sign-in, and ` · email` when riabuild
knows the sign-in's address and it fits. It is left out rather than cut. The rail starts
with three offers, the first sign-in of Claude Code, Codex and Grok Build. The chooser
adds more.

Emails are only ever known for Claude sign-ins. They arrive a moment after the window
opens. An unknown email shows nothing: the window never says "signed out".

### The pane

Drawn on a slightly raised background, with a two-column margin inside. Top to bottom:

1. A blank row.
2. **Status line**: the sign-in in bold, ` · email` if known, and on the right
   `<input> in / <output> out` token counts once the session has reported any.
3. A blank row.
4. **Conversation**, or the **splash** when the cursor is on an offer.
5. A blank row.
6. **Compose line**.
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
view stays pinned
to the newest line unless you scroll up. Changing rows or sending a prompt pins it to the
bottom again.

**Splash** (the cursor is on an offer), centred in the pane:

```
create a Claude session
login: claude-1 · ada@clubria.com
```

Only the harness name uses the accent colour.

**Compose line.** Always starts with `›`.

- Pane focused: your text with a `▏` caret. The terminal's own cursor is hidden.
- Rail focused, line empty: `press → to write`, muted.
- Rail focused, line not empty: the draft, muted. It is kept, not hidden.

There is **one** compose line for the whole window, not one per session. A draft stays
where it is while you move between rows, and Enter sends it to whatever row is selected
at that moment.

It is a single line. Pasted text is joined onto one line (see "Paste").

### The chooser

Pressing `n` on the rail opens a box titled ` new session ` over the body. It lists
every sign-in riabuild keeps: `claude-1` … `claude-N`, `codex-1` … `codex-9`,
`grok-1` … `grok-9`, in that order. Each row shows the sign-in and then its email if known,
or the harness name (`Claude Code`, `Codex`, `Grok Build`) if not. The list scrolls to
keep the cursor visible.

- It opens on the sign-in of the row you were on, so "another session like this one" is
  `n`, Enter.
- Enter **offers** the chosen sign-in: it is added to NEW SESSION (or found there if
  already present) and the cursor moves to it on the rail. No session is created.
- Esc closes it and changes nothing.

## Focus and keys

The keyboard talks to one of three places. `Ctrl-C` quits from any of them. Key releases
are ignored.

**Rail** (footer: `↑↓ move · → open · n sign-in · q quit`)

| Key | Does |
|---|---|
| `↓` `j` `Tab` / `↑` `k` `Shift-Tab` | next / previous row. Runs through sessions and then offers, and wraps around |
| `→` `Enter` | focus the pane. Typing works immediately |
| `n` | open the chooser |
| `1` `2` `3` | jump to the Claude / Codex / Grok offer |
| `q` | quit |

**Pane** (footer: `type to write · enter send · ^v paste · ↑↓ scroll · ← sessions`)

| Key | Does |
|---|---|
| any character | typed into the compose line at the caret. `q`, `n`, `j` and digits are just letters here |
| `Enter` | send the trimmed line and clear it. An empty line sends nothing. Focus **stays** in the pane |
| `←` | move the caret left. At the start of the line it goes back to the rail instead |
| `→` `Home` `End` | move the caret |
| `Backspace` / `Delete` | delete before / under the caret |
| `↑` `↓` | scroll the conversation one line |
| `PageUp` `PageDown` | scroll ten lines |
| `Tab` / `Shift-Tab` | select the next / previous rail row, with focus left in the pane |
| `Esc` | back to the rail. The draft is kept |
| `Ctrl-V` | paste |
| any other `Ctrl-` key | nothing. It does not type a letter |

**Chooser** (footer: `↑↓ account · enter choose · esc back`)

| Key | Does |
|---|---|
| `↓` `j` / `↑` `k` | next / previous sign-in, wrapping |
| `Enter` | offer it and return to the rail |
| `Esc` | return to the rail |

The mouse is captured and does nothing.

## What happens when you…

**Send to an offer.** A session is created under that sign-in and added to SESSIONS with
the cursor on it. Its title is the prompt, flattened to one line and cut at 60 characters.
Then the turn starts. The offer stays on the rail for next time.

**Send to a session.** `› text` appears at once and the session shows as working at once,
before the process has started. Any trouble mark is cleared. The turn continues the same
conversation under the sign-in the session was created with.

**Send while a turn is running.** Allowed. The prompt is queued and runs after the current
turn, in the order sent.

**Wait.** The window redraws about eight times a second. New output shows up as it is
written, and the spinner turns. Sessions the window did not start, meaning subagents, show
up within about three seconds, under their parent. The cursor stays on the row it was on.

**Hit a failure.** If a turn fails, or its harness will not start, the reason appears in
red in the conversation and the session's mark becomes `▲`. It **stays** `▲` after the
turn ends and until you send that session another prompt. If a session cannot be created
at all, nothing happens and the offer stays where it was.

**Paste (`Ctrl-V`).**

- An **image** on the clipboard (PNG preferred, then TIFF) is saved under riabuild's own
  directory, never in the checkout. Its **path** and a space are inserted at the caret, as
  ordinary editable text. That path is what the agent reads.
- Otherwise **text** is inserted with every run of whitespace, newlines included,
  collapsed to one space.
- An image wins when the clipboard holds both.
- On a server reached with `riabuild remote`, the clipboard read is the laptop's.
- `Cmd-V` / `Ctrl-Shift-V` are the terminal's own paste and never reach this handler.

**Quit** (`Ctrl-C` anywhere, or `q` on the rail). The screen is cleared and the shell
comes back as it was. Running turns are **not** stopped.

**Reopen.** Every session in this checkout comes back with its full conversation replayed,
running turns show as working, and failures from earlier are still shown.

## Notices

When `Ctrl-V` cannot do anything, the footer is replaced by one line in the warning
colour:

- `Nothing on the clipboard to paste.`
- the command to install a clipboard tool, on a Linux machine that has none
- the error, if reading the clipboard failed

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
  prompts, `›`, chooser border), ok/green (idle, tool succeeded), busy/orange (working,
  tool running, the "working" count), danger/red (trouble, failed tool, failure text),
  warn (notices), muted (headings, sign-ins, unselected titles, secondary text), strong
  (selected title, tool names, key names). Nothing uses a hard-coded colour, and
  `NO_COLOR` turns colour off.
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
- **Resize**: the layout recomputes on every frame, and nothing needs restarting.
