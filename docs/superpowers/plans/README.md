# Implementation plans — index

**Every plan in this directory is finished work.** Nine of them, eighteen thousand lines,
and not one of them is a queue. They are kept because the reasoning inside them is worth
more than the checklist wrapped around it: a plan records which alternatives were tried
and rejected on the way to the code, and that is the part the code cannot say for itself.

Read that sentence before the boxes, because the boxes disagree with it. Five hundred and
eighty-one `- [ ]` remain unticked against seventeen ticked, which is not a record of
outstanding work — it is a record of nobody going back to tick a box after doing the
thing. Every one of these features is in production. Marking each file completed at the
top was chosen over editing five hundred and eighty-one checkboxes by hand for exactly
that reason: the boxes were never the truth, so correcting them one at a time would be
paying a large cost to make a misleading artefact look tidy.

Each file also opened with an instruction to an agentic worker to implement the plan
task-by-task with a sub-skill. That line has been removed from all nine. It was the one
thing here that could cause harm rather than confusion — an agent that obeyed it would set
about rebuilding a shipped feature, and in the channel's case would rebuild a transport
that has since been deliberately replaced.

## The plans

Dates are when the work landed on `main`, which for every one of these is the same commit
that added the plan file — these were written and merged alongside the implementation
rather than ahead of it.

| Plan | Shipped | Design | What it built |
|---|---|---|---|
| [`2026-08-05-tui-console.md`](2026-08-05-tui-console.md) | #10, 2026-08-05 | [`tui-console`](../specs/2026-08-05-tui-console-design.md) | the dashboard as a single framed fake terminal, on the `src/ui` component library the Playwright suite can see |
| [`2026-08-05-async-rust-migration.md`](2026-08-05-async-rust-migration.md) | #13, 2026-08-06 | [`async-rust-migration`](../specs/2026-08-05-async-rust-migration-design.md) | the CLI on a current-thread tokio runtime, every IO trait behind `async_trait` |
| [`2026-08-06-claude-accounts.md`](2026-08-06-claude-accounts.md) | #28, 2026-08-07 | [`claude-accounts`](../specs/2026-08-06-claude-accounts-design.md) | up to nine Claude Code accounts, each with its own `CLAUDE_CONFIG_DIR` and launcher |
| [`2026-08-06-remote-mode.md`](2026-08-06-remote-mode.md) | #33, 2026-08-09 | [`remote-mode`](../specs/2026-08-06-remote-mode-design.md) | `riabuild remote`: provisioning a server over SSH and opening a mosh shell on it |
| [`2026-08-07-laptop-channel-and-clipboard.md`](2026-08-07-laptop-channel-and-clipboard.md) | #27 then #35, 2026-08-09 | [`clipboard-channel`](../specs/2026-08-07-clipboard-channel-design.md), [`exec-channel-transport`](../specs/2026-08-13-exec-channel-transport-design.md) | the laptop channel — clipboard and browser from a remote session. **Its transport was replaced on 2026-08-13**, so this plan is two changes behind the code |
| [`2026-08-12-subdued-child-output.md`](2026-08-12-subdued-child-output.md) | #48, 2026-08-12 | [`subdued-child-output`](../specs/2026-08-12-subdued-child-output-design.md) | a pty for noisy children and one dim role for their output |
| [`2026-08-12-concurrent-run-safety.md`](2026-08-12-concurrent-run-safety.md) | #53, 2026-08-12 | [`concurrent-runs`](../specs/2026-08-12-concurrent-runs-design.md) | two `riabuild` runs in two terminals, safe — the file lock around state |
| [`2026-08-12-shared-servers.md`](2026-08-12-shared-servers.md) | #60, 2026-08-13 | [`shared-servers`](../specs/2026-08-12-shared-servers-design.md) | the team's servers, entered once in the dashboard and read by every CLI |
| [`2026-08-13-issued-ssh-keys.md`](2026-08-13-issued-ssh-keys.md) | #73, 2026-08-13 | [`issued-ssh-keys`](../specs/2026-08-13-issued-ssh-keys-design.md) | an SSH key a lead issues, held in an `ssh-agent` riabuild owns and never on disk |

## Where the current answer lives

A plan is the least current thing in this repository, by construction: it describes what
was about to be done, and everything since has been done on top of it. When they
disagree, the order is the code, then the design spec in [`../specs/`](../specs/) — every
one of which now carries a **Date** and a **Status** — then `CLAUDE.md`, then the plan.
Nothing here should be cited as how riabuild behaves today.
