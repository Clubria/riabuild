# The agents window, redesigned

**Status:** implemented
**Date:** 2026-09-02

`docs/superpowers/specs/2026-08-24-riabuild-agents-design.md` built the machinery: sessions
on disk, one child per turn, liveness as a lock, twenty-seven sign-ins reachable. This spec
is about what the window *said*, which turned out to be wrong in one specific way, and about
the visual and keyboard decisions that followed from fixing it.

## The bug at the centre: a window that had been asked nothing said "3 sessions"

The first version opened a pane per harness on the way in. That reads as a reasonable
convenience — Claude, Codex and Grok are all there, pick one — and it is the source of five
separate complaints:

- The header counted three sessions in a checkout where no agent had ever been asked
  anything, because there *were* three sessions; `store.create` had run three times.
- Three directories appeared under `<root>/agents/` per checkout on the first open, and
  another three in the next checkout, for ever, whether or not a word was typed.
- A pane that is really an offer said **"waiting for the first reply"**, which is a claim
  about a conversation that does not exist.
- Nothing on the rail distinguished "a conversation you had yesterday" from "a button".
- And the chooser bound to `n` was the only way to reach `claude-2`, even though the rail
  was already a list of exactly the same kind of thing.

The fix is one distinction carried everywhere: **an offer is not a session.** `App` holds
`panes: Vec<Pane>` and `offers: Vec<Account>` as two different types, and the cursor is a
`Row { Session(usize), Offer(usize) }` over the concatenation. Offers have no id, no
directory and no transcript. `counts_line` counts `panes` only. The rail draws them under a
separate `NEW SESSION` heading with a `+` mark rather than a state dot.

**A session comes into existence in exactly one place**: `drive::send`, when the cursor is on
an offer and there is text to send. That is also the honest definition — a session is a
conversation, and a conversation starts when somebody says something.

### Upgraded machines have to be cleaned up, not just spared

Every existing install already has three untouched directories per checkout, so shipping the
new rule alone would leave the old bug on every machine that had ever run the old window.
`drive::restore` forgets a record whose title is empty, whose spool is empty, whose
`errors.log` is empty and whose lock is free. All four together mean nothing was ever said,
nothing failed and nothing is running, so there is provably no conversation to lose. A
record failing any one of them is kept.

## Separation is a background, not a line

The rail and the pane were divided by a vertical rule. They are now divided by the pane
having a **slightly raised background** and the rail having none — the terminal's own
background shows through on the left. This is the general pattern, not a local choice: it is
the same relationship `riabuild-web`'s surfaces have to the page.

`Theme` grew three things for it. `Tone` (dark or light, read from `COLORFGBG` and defaulting
to dark) says which pair of constants to use, because a raised surface on a light terminal is
*darker* than the page and lighter on a dark one. `Theme::surface()` returns a `Style`
carrying both a background and a foreground — both, because setting a background without an
ink colour inherits whatever the terminal's foreground happens to be, which on a light
terminal is black text on a near-black panel.

And `Theme::has_surfaces()` is the honest half: below `Depth::Ansi256` there is no colour
close enough to the terminal's background to read as *slightly* raised — the nearest
sixteen-colour options are "the same" or "a completely different colour". So on a
sixteen-colour terminal `surface()` is empty and `frame.rs` draws a muted vertical rule in
the gutter instead. The fallback is a different design, not a degraded one.

## Margins, and the row that is deliberately empty

Two columns on the left and right edges of everything, and a blank row above the header with
no background at all. The blank row is what makes the window read as *inside* the terminal
rather than as having replaced it, and it is drawn as ordinary background for the same
reason the rail is: it belongs to the terminal.

`frame.rs` owns all of this and nothing else — a six-row vertical layout, an `inset()` that
takes two columns off each side, the rail/pane split, and the pane's own inner layout. It
was split out of `lib.rs` so that file is the keymap and the terminal and nothing more.

## The input box lives in the pane

It was a screen-wide bar under everything, separated by a rule. It is now inside the pane,
inset by the same two columns, one blank row above and below it, with no rule and no
background of its own — it is part of the surface it sits on. A box that spans the window
implies it belongs to the window; it belongs to one session.

## A pane with no session behind it says what it would be

An offer's pane has no transcript, so `transcript_lines(None, ..)` returns nothing at all and
the pane draws a centred two-line splash instead:

```
              create a Claude session
     login: claude-1 · ada@clubria.com
```

The harness name is in `Role::Brand`, the label `login:` and the email are `Role::Muted`, the
account name is `Role::Strong`. It answers the two questions an offer actually raises —
which tool, and signed in as whom — and it replaces "waiting for the first reply", which was
a sentence about a conversation that had not been started.

## The keyboard: two places, and one keypress to get between them

There were three focuses: the list, the transcript, and the compose box. Reaching the box
took Enter twice — once to enter the session, once to start typing — and the second press was
invisible, because a transcript and a composer look the same when you are looking at a
transcript.

There are two now. `Focus::List` and `Focus::Session`, and in a session **every printable
character goes into the box**. `→` or Enter from the rail opens a session and you are already
typing. `←` leaves for the rail *only* when the caret is at position 0; otherwise it moves the
caret, which is what `←` means in every text field there has ever been. Escape leaves from
anywhere. Enter sends and **stays** in the session, because the next thing a developer does
after asking something is ask a follow-up.

The chooser (`n`) survives as a third focus because it is a modal, not a place.

## Which repository, said out loud

The store was already scoped to the checkout — `store.sessions(&cwd)` filters on
`record.cwd` — so this was a missing *statement*, not a missing behaviour. `Request` carries
`repo: Option<String>` from `Ctx::repo()`, and the header reads `riabuild agents` in
`Role::Brand` beside `Clubria/riabuild` in `Role::Strong`. `None` renders as nothing rather
than as a guess: a checkout with no GitHub remote is an ordinary thing to run this in.

## Sign-ins arrive after the window does

Showing an account's email meant asking `claude auth status --json`, which costs about
450 ms of child-process startup **per account**. Twenty-seven of those before the first
frame is a blank terminal for a second and a half, which is a worse window than one with no
emails in it.

So `dispatch` spawns the probes and hands `agents::run` an
`UnboundedReceiver<Login>`; the loop's `select!` gained an arm that fills them in as the
children answer. An unknown email renders as **nothing** — never as "signed out", which
would be a claim about the account that nothing established, the same rule
`accounts::status::Identity::Unknown` exists to enforce.

They live in a side table on `App` (`logins: Vec<(Kind, usize, String)>`) rather than on
`Account`, so `Account`'s `PartialEq` keeps meaning "the same sign-in" rather than "the same
sign-in, and we had both learned the same amount about it".

The rail appends ` · email` to an offer only when the whole thing fits; when it does not, the
email is dropped rather than truncated, because half an address is worse than none. Every
session's status line carries `claude-2 · ada@clubria.com` on the left and the token counts
on the right.

## Ctrl-C has to clear the screen — twice

The previous spec covered `claim()`: ratatui writes only cells that differ from the frame
before, so the first draw leaves every untouched cell showing whatever was on the alternate
screen. The other half was missing.

On the way *out*, `release()` clears and flushes **before** `LeaveAlternateScreen`. On a
terminal that honours the alternate screen this is invisible — leaving it restores the shell
scrollback either way. On one that does not, and there are several ordinary ones (tmux with
`alternate-screen off`, `screen`, any `TERM` whose terminfo has no `smcup`/`rmcup`), the
interface is drawn on the *main* screen, over the developer's shell history, and leaving
does nothing at all: they get their prompt back underneath a full-screen interface that is
still on the display.

A chained panic hook does the same job for the abnormal exit, because a panic in a raw-mode
alternate screen otherwise leaves a terminal that does not echo.

## `--exclude-dynamic-system-prompt-sections`

Every generated launcher passes it. The `-p` turn this window runs did not, which made the
agents window the one way to reach Claude Code on a Clubria machine that defeated prompt
caching on every turn of every session — the dynamic sections change between launches, so
carrying them invalidates the cache each time, and there is no settings key for it, only the
flag.

The bare interactive launcher has to work around an `agents` positional the flag can fall
through into; `-p` has no such positional, so it is an ordinary option line here. A test pins
it for both the fresh and the resumed argv, and that the prompt is still last.

## Files

`agents` was one 800-line `lib.rs` and one 800-line `draw.rs`. It is now:

| File | What |
|---|---|
| `lib.rs` | the terminal, the keymap, `Request`/`Login`/`Action` |
| `frame.rs` | the layout: margins, the rail/pane split, the surface, the popup |
| `draw.rs` | line builders — no widgets, so what the screen says is assertable |
| `drive.rs` | the loop and the four things it does between frames |
| `compose.rs` | a character-indexed single-line editor |
| `app.rs` | the pure state: panes, offers, cursor, focus, logins |

`draw.rs` and `app.rs` sit slightly over the ~300-line guideline. Both were left whole: the
alternative is splitting one coherent thing across two files to satisfy a line count, which
is the cost the guideline exists to avoid rather than the one it charges.

## Testing

`frame.rs` renders into a `TestBackend` and asserts on the buffer, which is the only way to
test a decision that *is* a background: `buffer[(4, 10)].bg == Color::Reset` on the rail and
`buffer[(60, 10)].bg == surface` in the pane is the whole of item five. A second test forces
`Depth::Ansi16` and asserts the rule appears in the gutter instead.

The keyboard tests are written as the complaint was: one keypress reaches the box and the
next character lands in it; `←` moves the caret until there is nowhere left to go and then
leaves.
