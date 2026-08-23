# Choosing and moving the project checkout

**Date:** 2026-08-06
**Status:** Implemented

## Why

riabuild clones the Clubria repository to whatever `default_project_path` riabuild-web
reports, and there is no way to change it afterwards short of hand-editing
`~/.riabuild/config.json`. Two consequences:

- A developer with an existing layout (`~/src`, an external drive, a work volume) gets a
  checkout somewhere they did not want it and cannot say so.
- Once the path is recorded, moving the checkout silently breaks riabuild: every later
  run finds the recorded directory gone and re-clones to the old location.

This adds one deliberate decision to first setup, and a command that makes it revisable.

## Scope note

The root `CLAUDE.md` frames riabuild as getting a developer set up "without them making a
single environment decision". This change adds exactly one, deliberately: where their own
source code lives is the decision developers most reliably have an opinion about, and
getting it wrong is the one riabuild cannot quietly fix later. The default still comes
from the server and Enter still accepts it, so the zero-decision path is intact for
anyone who does not care. `CLAUDE.md` and the original design doc are updated to say so.

## Interactivity

The CLI has never read from stdin. `Ui` gains one method:

```rust
/// Reads one line from the developer. `None` means "no answer" — either they
/// pressed Enter, or there is no terminal to ask.
pub fn ask(&self, question: &str) -> Option<String>
```

`Ui` carries an `interactive` flag set from `stdin().is_terminal() &&
stdout().is_terminal()`. Both halves matter: a piped stdin with a terminal stdout is
exactly the CI shape where a blocking read hangs a build until it times out. When
`interactive` is false, `ask` returns `None` without printing, and every caller must have
a default — a prompt is never the only way to reach an answer.

EOF and an empty line both return `None`, so ^D behaves like Enter.

`--quiet` does not suppress prompts. Quiet means "only print what needs attention", and a
question is the definition of that.

Under `cfg(test)` interactivity is forced off unless the test constructs
`Ui::scripted([...])`, which serves canned answers from a queue. Without that, `cargo
test` run from a real terminal would block on the first prompt.

`--check` and `status` never prompt, and this needs no special handling: the engine skips
`apply()` entirely in dry-run mode (`tasks/engine.rs`), and every prompt lives in an
`apply()` or in a subcommand of its own.

## First-setup prompt

`Project::apply` only asks when `config.project_path` is unset — that is, on the first
run, and not when `--project` was passed. The wording:

```
The repository will be installed at ~/code/ai-builders-hub. Choose a different path? (press enter for default)
```

The default is `paths::default_project_dir` — riabuild's own platform-dependent answer
(`~/Documents/Clubria/<repo>` on macOS, `~/code/<repo>` elsewhere), which replaced a
server-sent string that could not be right on both platforms at once. Asking does not
move that decision back to the server; it only lets the developer override it.

A typed answer is validated before it is accepted:

- it must be absolute, or start with `~/`
- it must not be an existing non-empty directory (an existing checkout of the right repo
  is fine — `apply` already handles adopting one)

An invalid answer is re-asked, up to three attempts, after which the default is used with
a note. Non-interactive behaviour is unchanged from today: take the default, print the
existing note.

## `riabuild move-project [PATH]`

```
$ riabuild move-project
  The repository is at ~/code/ai-builders-hub.
  Move it to: ~/work/hub
  moved ~/code/ai-builders-hub → ~/work/hub

$ riabuild move-project ~/work/hub
```

The destination may be given as an argument, which makes the command scriptable and
usable over a non-TTY session; without one it prompts. Non-interactive with no argument
is a failure that names the argument form.

Order of operations:

1. Require a recorded `project_path` whose directory exists. Neither is something the
   command should invent — `riabuild` is what creates a checkout.
2. Resolve the destination, expanding `~`. It must be absolute.
3. Refuse three shapes: the same path, a destination that already exists and is not an
   empty directory, and a destination nested inside the source (a recursive copy into
   itself).
4. `create_dir_all` the destination's parent, so a path through directories that do not
   exist yet works.
5. Move the tree (below).
6. Record the new path in `config.json`.
7. Report the move. If the developer is inside the riabuild shell, warn that its working
   directory now points at a directory that no longer exists.

## Moving the tree

`fs_move::move_tree` is `rename`, falling back to copy-then-delete on
`ErrorKind::CrossesDevices`. The fallback exists because a developer moving a checkout to
an external drive or a second volume is exactly the case a bare `rename` cannot serve.

Two properties the fallback must have:

**Symlinks are recreated, not followed.** `node_modules` under pnpm is mostly symlinks
into a virtual store. `fs::copy` on a symlink copies its target, which either duplicates
the whole store into the destination or fails outright on a link whose target has gone.
The copy walk uses `symlink_metadata` and recreates links with `std::os::unix::fs::symlink`.

**The source is deleted only after the copy is verified.** Verification is that the walk
completed without error and that the destination holds as many entries as the source. If
anything fails partway, the partial destination is removed and the source is left
untouched — a failed move must never be a lost checkout. That the source was a real
checkout is established before the move starts, not after it.

## Interaction with `--check`

`--check` is global and documented as changing nothing, so `move-project` honours it:
the destination is resolved and every refusal is reported, and then the move is described
rather than performed. A global flag that silently did not apply to one subcommand would
make it untrustworthy on all of them.

## Files

| File | Change |
|---|---|
| `src/ui.rs` | `ask`, the `interactive` flag, `Ui::scripted` for tests |
| `src/cli.rs` | `MoveProject { path: Option<String> }` |
| `src/main.rs` | one dispatch arm |
| `src/tasks/project.rs` | prompt in the unset branch of `apply` |
| `src/move_project.rs` | new — the command's flow and its guards |
| `src/fs_move.rs` | new — `move_tree`, the copy fallback |

`Project::version()` is not bumped. `check()` is unchanged, and machines that already
have a path recorded have nothing to redo.

## Testing

- `fs_move`: rename within a filesystem; the copy walk over nested directories, a file,
  and a symlink, asserting the symlink is still a symlink; a failed copy leaves the
  source intact.
- `move_project`: the three refusals, and that a successful move rewrites
  `config.project_path`.
- `ui`: `ask` returns `None` with no terminal, serves scripted answers, and treats an
  empty line as no answer.
- `tasks::project`: a scripted answer is used in place of the default; no terminal falls
  back to the default.
