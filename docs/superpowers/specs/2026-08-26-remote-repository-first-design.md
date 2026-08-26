# Asking which repository before connecting

**Date:** 2026-08-26
**Status:** Implemented

## Why

`riabuild remote` has always put two questions. Which server, on the laptop, first. And
which repository — on the *server*, minutes later, because the laptop runs the server's
own riabuild over `ssh -t` and that riabuild is the one with the picker in it.

Minutes is not an exaggeration of the gap. Between the two answers sit a host key to pin,
riabuild's key to authorise (which on a new server asks for the account password and runs
`ssh-copy-id`), a riabuild to download and install on the far side, a 90-day session to
mint, and a GitHub sign-in to lend. The developer commits to all of it before being asked
the one thing that decides what the box is *for* — and then answers it from inside a
connection they can no longer cheaply change their mind about. Setting `payments` up on
the wrong server means `remote forget`, or living with it.

It also reads wrong at the terminal. Two questions about one run arrive at opposite ends
of the longest wait riabuild ever puts a developer through, which is not how a run that
opens with "which server?" implies it will go.

So both questions are asked on the laptop, back to back, before the first `ssh`, and the
answer travels to the server the way a repository named on the command line already did.

## What the developer sees

```
Your servers:
  1  build-01     used 2h ago
  2  shared-gpu   the team's · used yesterday
  3  Add a server

Which one? [1 · build-01]

Clubria repositories:
  1  ai-builders-hub    default · pushed 2h ago
  2  payments           pushed yesterday
  3  design-system      pushed 3d ago
  … 6 more — type a name to work on one of those

Which repository on build-01? (press enter for payments)

▸ Connecting to ada@build-01.fly.dev
```

The second question is the picker `riabuild` itself puts, unchanged in what it accepts: a
number, a name, Enter. Two things about it are new, and both come from its being asked
about a machine other than the one it is typed on.

**It names the server.** "Which repository?" asked at a laptop's own terminal reads as a
question about the laptop. `pick::Offer.on` is what puts `on build-01` into it — the same
reason the question already names what Enter would take rather than leaving it to the box
above, which `--quiet` drops.

**No row is marked `cloned`.** That marker means "this machine already has a checkout",
and this machine is not the one that will clone. A laptop cannot see what a server has,
so the honest answer is to mark nothing rather than to print a guess about a filesystem
nothing has looked at.

## What Enter takes

The repository this laptop last set **that server** up for, then the one this laptop is
itself working on, then the org default.

The first of those is new state, and it exists because the change would otherwise take
something away. The server's own picker offered the server's own last choice — a
developer whose laptop is on `ai-builders-hub` and whose GPU box is on `payments` pressed
Enter twice and got both. Now that `--repo` is always passed, the server's memory is never
consulted, so pressing Enter would quietly move the server onto whatever the laptop was
doing. The memory has to move with the question:

```rust
/// The repository this laptop last set this server up for, as an `owner/repo` slug.
#[serde(default)]
pub repo: String,
```

on `store::Record`, written by `store::remember` — which is what a *successful* connect
leaves behind, so a run that failed on the way there has changed nothing. `#[serde(default)]`
covers every `remotes.json` written before the field existed: empty means "ask", not a
wrong guess.

It is a memory of an answer and never an authorization. What a developer may work on is
GitHub's to say, asked through their own `gh` every time the box is drawn — the picker's
own rule, unchanged, and the reason riabuild holds no permission logic that could be wrong
about it.

## Where nothing is written

`repo::pick::choose` records what it settled on in this machine's `config.json` —
`active_repo`, and the checkout migration beside it. That is right for a run about this
machine and exactly wrong for one about a server: `riabuild remote gpu` would leave the
laptop working on whatever the server was told to.

So the question splits in two, the way `remote::pick` already splits deciding from acting:

```rust
/// The box, the question, and nothing written down.
pub async fn offer(ctx: &Ctx, offer: Offer<'_>) -> Repo

pub struct Offer<'a> {
    pub default: &'a Repo,
    pub org_default: &'a Repo,
    pub known: &'a BTreeMap<String, String>,
    pub on: Option<&'a str>,
}
```

`choose` is `offer` plus `adopt`, so the local flow is unchanged to the line. `remote::repo`
calls `offer` and records the answer where it belongs — beside the server in
`remotes.json`, and on the wire as `--repo`.

## What is not asked

**`--repo` on the command line.** Already an answer; asking would be putting a question
whose answer is in the argv it was typed in. It is passed through as before.

**`--check`.** The server is run with `--check --no-shell` and nothing else, so an answer
would reach nothing — and a question is a poor thing to put to somebody who asked for a
report. `repo::pick`'s rule and `provision`'s, applied here.

**A run with no terminal.** `Ui::ask` answers `None`, and the remembered repository
travels anyway, because it is not a guess: it is what this laptop set this same server up
for last time. With nothing remembered, nothing is sent and the server decides as it
always did — which is also what a caller that never reached `flow::run`'s `connect` gets,
since without org configuration there is no owner to list and no default to offer.

Note what this does *not* change: `remote::pick`'s refusal to guess a **server** when
nobody is there stands. Connecting provisions a machine; picking a repository for it does
not.

## Where the checkout goes

Still asked on the server, by the server, and that is correct rather than an omission:
`project::choose_dir` is a question about a filesystem this laptop cannot see. It stays
where the directory it is about is.

## The one flag that had to become testable

Everything between "which server" and the shell needs a real server to exercise, so the
argv the server is run with was assertable only by reading it. `--repo` no longer comes
straight off `Request` — it is what `repo::choose_for` settled on — and a resolved value
that quietly failed to reach the argv would compile, connect, provision, and leave the
server on the repository it already had, with the developer's answer discarded silently.
`flow::connect::setup_args` is that line, pure and pinned by three tests.

## Files

| File | What |
|---|---|
| `tasks/src/repo/pick/mod.rs` | `offer` and `Offer` split out of `choose`; the question names the machine it is about |
| `remote/src/repo.rs` | which repository a remote run is about, and the four cases where the laptop says nothing |
| `remote/src/store.rs` | `Record.repo` — the memory the server's own picker used to keep |
| `remote/src/store/persist.rs` | `remember` records it, and a `None` leaves it alone |
| `remote/src/flow/connect.rs` | the question, directly after the server and before the first `ssh`; `setup_args` |

Supersedes "Remote mode asks once, on the server" in
`2026-08-18-repository-picker-design.md`.
