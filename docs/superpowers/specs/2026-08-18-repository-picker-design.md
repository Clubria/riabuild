# Picking a repository

**Date:** 2026-08-18
**Status:** Implemented

## Why

riabuild is single-repository by construction. `OrgConfig.repoSlug` is one string a lead
sets in the dashboard, `UserConfig.project_path` is one path, and the `project` task
actively *refuses* a checkout whose `origin` names anything else — "that checkout points
at X, not Y". A developer whose work is in a second Clubria repository has no path
through riabuild at all: they clone by hand, and everything riabuild exists to do for the
first repository — the toolchain, the brokered `.env` files, the trusted Claude directory
— stops at the edge of it.

So `riabuild` asks which repository to work on, and `ai-builders-hub` is what Enter
means. The choice joins the checkout path as the second thing riabuild *offers* rather
than imposes, and for the same reason: a developer who presses Enter has still decided
nothing.

## What the developer sees

Every provisioning run, after sign-in and before any checks:

```
Clubria repositories:
  1. ai-builders-hub    default · cloned · pushed 2h ago
  2. payments           cloned · pushed yesterday
  3. design-system      pushed 3d ago
  4. riabuild           pushed 5d ago
  … 6 more — type a name

Which repository? (press enter for payments)
```

Enter takes the active repository — the one this machine used last, and the org default
on a machine that has never chosen. A number picks from the box. A name picks anything
at all this developer can see: `payments`, `Clubria/payments`, or another owner's
`owner/repo`.

Ordering is the active repository, then the ones already cloned on this machine, then the
rest by most recently pushed, capped at ten lines. `cloned` is there because switching
back to a checkout that already exists costs nothing, and a developer cannot tell which
those are from a list of names.

Three unusable answers and riabuild takes the default, saying so — the bound
`project::choose_dir` and `remote::pick` already use, for the reason they already state: a
developer who cannot give a usable answer is better served by riabuild choosing than by
being asked forever.

## What riabuild remembers

`UserConfig` grows a map and a pointer:

```rust
/// Checkouts by `owner/repo`. Absolute once chosen.
pub repos: BTreeMap<String, String>,
/// Which of `repos` this machine is working on.
pub active_repo: Option<String>,
```

Two repositories therefore mean two checkouts, side by side, each keeping its own
branches, its own uncommitted work and its own `.env` files. Switching back to one is
silent: the path is remembered, so nothing is re-cloned and nothing is asked.

`active_repo` is written by whatever decided — the picker, or `--repo`. The `project` task
fills it only when it is *blank*, and never overrules it: that is the first run above,
where nothing put the question, and a machine that recorded a checkout while saying it
works on no repository would be a file that contradicts itself.

### The old single path

`project_path` stays, and stays serialised, until it is migrated. It cannot be folded in
`UserConfig::load` the way `claude_profile` is, because folding it means knowing *which
repository* that path was a checkout of, and the org default is not in hand until the
config has been fetched. So the migration happens in the picker — the first place both
facts are known — and clears the field in the same write.

Leaving it serialised until then is the whole point. `skip_serializing` would mean any
run that rewrites `config.json` for an unrelated reason — `riabuild claude add` is one —
drops a checkout nothing has folded yet, and the next run clones a second copy of the
repository the developer already has.

`Ctx::project_dir` reads the map, keyed by the repository *this run* is about rather than
by `active_repo` — which repository a run is about is the run's to know, and `active_repo`
is only how that is remembered for the next run's default.

It falls back to `project_path` under two conditions together: the repository asked about
is the org default, and nothing has chosen yet. Both are about never handing back a path
that is a checkout of something else — the old path can only be a checkout of the default,
and a machine that has chosen has a map. That is what lets `riabuild status`, `riabuild
env`, `riabuild shell` and `riabuild --check` find an existing checkout on a machine whose
migration has not run, none of which may write. Read both, write one.

### No per-repository task state

None, and no state migration either. `engine::status_for` calls `task.check(ctx)` on every
run; the record in `state.json` gates version bumps and upstream changes, never the check
itself. So switching the active repository means `project`, `env_local`, `claude_trust`,
`claude_plugins` and `repo_status` each look at the newly active checkout, find it missing
or wrong, and repair it — which is what their `check()` already does. The invariant that
makes this free is the one `engine.rs` already states: state is riabuild's memory, not the
machine's state.

## Where the slug comes from

`OrgConfig.repo_slug` stops being *the* repository and becomes **the org default** — the
value Enter takes, and nothing else. Everything downstream reads the run's active
repository instead.

A validated newtype replaces the bare string:

```rust
pub struct Repo { slug: String, slash: usize }   // always exactly "owner/name"

impl Repo {
    /// As `/api/v1/org/config` serves it: the owner is not optional.
    pub fn parse(raw: &str) -> Result<Repo>;
    /// As a developer types it: a bare name means "in our org".
    pub fn parse_with_owner(raw: &str, default_owner: &str) -> Result<Repo>;
    pub fn slug(&self) -> &str;
    pub fn owner(&self) -> &str;
    pub fn name(&self) -> &str;                          // was OrgConfig::repo_name
    pub fn matches_remote(&self, remote: &str) -> bool;  // was OrgConfig::matches_remote
}
```

Two constructors rather than one with an `Option`, because the two callers differ in what
a missing owner *means*. A developer typing `payments` is being helped; a dashboard slug
naming no owner is a value that would clone whichever `payments` the signed-in account
happens to own, and the old `repo_name()` let exactly that through.

`parse` is the security-shaped part of this design, and the reason the newtype exists at
all rather than another `String`.

The first draft of this spec said `repoSlug` was validated on the server and that the
picker was making a checked value into developer input. That was wrong, and the truth is
worse: `org.update` validates `minCliVersion` and `latestCliVersion` against a regex and
stores `repoSlug` as a bare `v.string()`. So a lead could already type anything into the
dashboard and have it arrive on every developer's machine, and this feature is what
noticed. Both ends are checked now — the server refuses the write, the CLI refuses the
value — and neither makes the other redundant: one keeps a lead's typo off every machine,
the other is what makes an answer typed at a prompt, which never passes through the
server, safe.

What the value reaches, on both paths:

- `gh repo clone <slug> <dir>` argv, where a leading `-` is a flag rather than a
  repository, and
- a **directory name**, via `paths::default_project_dir(home, repo.name())`, where `..` or
  a separator puts a checkout — and the brokered `.env` files written into it — somewhere
  the developer never named.

So `parse` accepts exactly one `/`, refuses an empty half, refuses anything outside
`[A-Za-z0-9._-]`, refuses a component that is `.` or `..`, and refuses a leading `-` in
either half. The reasoning is `org::version_only`'s, one step further: that check exists
so the CLI survives a server that forgets its own, and here there is no server check to
forget, because the server never saw the value.

A bare name is completed with the default owner, so a developer types `payments` and not
`Clubria/payments`. The owner comes from the org default's own owner — no new field on
`/api/v1/org/config`, and nothing new crossing the data/logic boundary.

## Listing repositories

One request, made as the developer, through the `gh` riabuild owns:

```
gh api "orgs/<owner>/repos?type=all&sort=pushed&per_page=30" --jq …
```

GitHub does the filtering. The token is the developer's own, so the list *is* what they
are authorized to see, and there is no permission logic anywhere in riabuild to get
wrong. The alternative — riabuild-web listing with `GITHUB_ORG_TOKEN` — would need one
request per repository per member, because GitHub has no "repositories visible to user X"
endpoint, and its answer could still disagree with what the developer's own `gh` will
clone.

No cache file. One round trip per run, bounded by a timeout, beats a TTL, a staleness
rule and the tests they need. `per_page=30` without `--paginate` keeps it one request on
an org of any size; anything past the listed ten is reachable by typing its name.

Three failure modes, kept distinct, because "we could not tell" must never render as "you
have no repositories" — the distinction `github.ts` already draws with `unavailable`:

| Situation | What the prompt does |
|---|---|
| `gh` not installed yet | offers the default, says the list needs GitHub sign-in and will be there next run |
| `gh` present, the listing failed or timed out | offers the default, names the failure |
| the listing worked and was empty | says so, rather than drawing an empty box |

In all three a typed name still works, and the run continues. A repository list is not
something a provisioning run may fail on.

## Where the prompt sits

In `provision`, between `ctx.connect()` — which is what makes the org default known — and
`engine::run_all`, so every repository-scoped task sees the answer.

`--check`, `status`, `env` and `shell` never ask. A dry run must change nothing, and
`config.json` is part of nothing; the other three do less than provision by definition.

**A machine with no session yet is not asked either**, which is every machine's first run.
The question needs two things that arrive with a session — a default to offer, and a
GitHub sign-in to list through — and neither exists before the `login` task has run, which
happens inside the task engine the question is put in front of. So the first run
provisions the org default and records it, and the run after it, which has both, puts the
question. The alternative was splitting the engine in two around one prompt: a structural
change, to buy a list on the one run that cannot have one anyway.

**No terminal takes the default, silently.** `Ui::ask` answers `None` there and taking the
default is the crate rule. `remote::pick` is the deliberate exception to that rule
because connecting *provisions a server*, mints it a session and lends it a GitHub
sign-in; picking a repository is the same decision riabuild would otherwise have made
alone, so the rule holds here. This is also why both existing e2e suites keep passing
untouched — they already rely on it for `choose_dir`.

`--repo <owner/repo>` skips the prompt, a global flag beside `--project`, and is forwarded
to a server exactly as `--project` is: one field on `remote::Request`, two lines in
`remote::flow::connect`.

Remote mode asks once, on the server. `riabuild remote` runs the server's own `riabuild`,
so the question appears there over `ssh -t`, and the laptop adds no second one.

The answer is settled by a pure function, so every rule is testable without a test process
ever reading real stdin — the split `remote::pick` documents:

```rust
pub enum Answer {
    /// Enter: the repository the question offered.
    Default,
    /// A number, as a zero-based index into the rows shown.
    Listed(usize),
    /// A name, which may be any repository this developer can see.
    Named(Repo),
}

/// `Err` carries the objection to put before asking again.
pub fn settle(answer: &str, shown: usize, default_owner: &str) -> Result<Answer, String>
```

`Err(String)` rather than `None`, because `Repo::parse` already knows *why* a name is
unusable and the prompt should say that rather than "try again". Ordering and the ten-row
cut are pure too, in `rows_for`, for the same reason: what the box shows is a rule, and a
rule that can only be checked by drawing a box on a terminal is a rule nothing checks.

## Moving a checkout

`riabuild move-project` moves the checkout of one repository, and with more than one
possible it has to ask which before it asks where. It puts the same picker, restricted to
repositories this machine has actually cloned — a repository with no checkout has nothing
to move — and then the destination question it asks today.

The restriction is what makes the two prompts one thought rather than two: the box a
developer chooses from is the list of trees on this machine, so every option is something
`fs_move` can actually move. With exactly one cloned repository there is nothing to
choose and the question is not put at all, which keeps `riabuild move-project <path>`
behaving precisely as it does today on the machine every developer has.

Moving does not change which repository is active. It is a question about a directory.

A typed name has to be one of the checkouts, and `riabuild has no checkout of X` is the
answer when it is not — better said at the question than by a `rename` failing on a path
nobody recorded.

## What this costs, said out loud

- **Every checkout gets its own copy of the brokered secrets.** `env_local` writes
  `.env.<name>` into the active checkout, so two repositories mean two copies on disk.
  That is today's model unchanged rather than a new hole, but N repositories multiply it.
  The environments stay org-level: `secretEnvironments` does not become per-repository,
  and nothing about brokering changes.
- **A new repository asks the path question once.** `choose_dir` runs for a repository
  with no recorded path, and never again for that one.
- **The first run on a machine cannot show a list**, because `gh` is installed by a task
  that has not run yet. Accepted rather than worked around: on a fresh machine the org
  default is the answer anyway, and the run after it has the full list.
- **Attribution and isolation are unchanged.** `.riabuild-owner` markers stay
  per-checkout, and on a server every repository still lands under the developer's own
  directory, because `default_checkout` names the active repository inside the same
  per-developer namespace it already used.

## Surfaces

| Surface | Change |
|---|---|
| `api/src/org.rs` | `Repo`; `OrgConfig::default_repo()`; `repo_name`/`matches_remote` move onto `Repo` |
| `paths/src/config.rs` | `repos`, `active_repo`, the migration and its clear |
| `tasks/src/lib.rs` | `Ctx::repo()`; `project_dir()` reads the map; `default_checkout()` names the active repository |
| `tasks/src/repo/` | new — `list.rs` (the `gh` call), `render.rs` (the box), `pick.rs` (`settle` and the question) |
| `tasks/src/project.rs` | clones the active repository; the mismatch message names it |
| `cli/` | `--repo`; `provision` puts the question; `status` reports the active repository and the known checkouts; `move-project` asks which |
| `remote/` | `Request.repo`, forwarded as `--repo` |
| `riabuild-web` | no endpoint change. `org.update` validates `repoSlug` and stores it trimmed — see above. One label: `repo <slug>` becomes `default repo <slug>`, so a lead is not told it is the only one |
| `e2e/run.sh` | reads the checkout map rather than `project_path`, and one new step for `--repo` |
| docs | this spec; the root `CLAUDE.md` exception becomes two — where source lives, and which repository |

`/api/v1` is untouched. The server still ships one string and never a list it computed
permissions for.

## Out of scope

No lead-curated repository list — the dashboard names the default and nothing more. No
per-repository secret environments. No `riabuild repo` subcommand: the every-run prompt
*is* the switch, and a command to do the same thing is a second way to learn the same
idea. No working on two repositories in one shell.
