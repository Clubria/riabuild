# Which secrets a repository gets

**Date:** 2026-09-04
**Status:** Implemented

## Why

`riabuild` asks which repository to work on and then fills the same `.env.dev` and
`.env.staging` into whichever checkout it landed in. The folders those secrets come from are
`INFISICAL_SECRET_PATH`, **one list on the deployment**, and the environments are
`environmentsForRole`, **one list per role**. Neither knows which repository the run is
about.

So a team with more than one repository has exactly one set of secrets, and every checkout
gets a copy of it. `payments` receives the hub's Stripe keys; a repository with no secrets
at all receives them too, and `env_local` hard-fails a run where Infisical has nothing to
give — a repository that is *supposed* to have no environment variables cannot say so, and
the developer meets "Infisical returned no secrets" on a machine with nothing wrong with
it.

The repository-picker spec saw this and deferred it, in as many words:

> **Every checkout gets its own copy of the brokered secrets.** […] The environments stay
> org-level: `secretEnvironments` does not become per-repository.

and again under "Out of scope": *"No per-repository secret environments."* This spec is
the reversal of that decision, and it is worth being clear about what changed underneath
it. When it was written, the second repository was hypothetical — the picker was the thing
that made one reachable at all. It is not hypothetical now, and the sentence it bought
("nothing about brokering changes") turned out to buy a wrong answer rather than a small
one.

## What a lead sees

A table in the dashboard, beside the shared servers and the issued keys:

```
Secrets by repository

  repository                infisical folders                          environments          changed
  Clubria/ai-builders-hub   /tenant/aibuilders/frontend                dev · staging         2h ago
                            /tenant/aibuilders/convex
  Clubria/payments          /apps/payments                             dev · staging · prod  1d ago
  Clubria/design-system     — not mapped, riabuild writes no env files

  ▸ add a mapping
```

One row per repository, typed by a lead. **A repository with no row gets no environment
files at all** — that is the whole of what "unset" means, and it is a decision rather than
an oversight: riabuild says so on the run, once, and reports the task satisfied.

A repository takes **an ordered list** of folders rather than one, because one environment's
secrets are not always in one place — AI Builders' `dev` is `/tenant/aibuilders/frontend`
and `/tenant/aibuilders/convex`, and a `.env.dev` carrying either half alone does not start
the app. The order is the merge order and it is dotenv's own: **later wins**. That is the
contract `INFISICAL_SECRET_PATH` already carries, moved to where a repository can have its
own.

The `environments` column is not typed by anyone. It is what those folders actually have,
read from Infisical when the page loads, and it is there because a path is a value a lead
can typo and a folder listing is the only thing that can tell them so before every
developer's next run does.

## What riabuild does

| | Before | After |
|---|---|---|
| the folders | `INFISICAL_SECRET_PATH`, one list per deployment | the run's repository, from its row |
| the environments | `environmentsForRole(role)` — a hardcoded pair | the environments that folder exists in |
| a repository nobody mapped | the hub's secrets, into its checkout | nothing, said out loud |
| a repository with no secrets | a hard failure on every run | satisfied |

### The environments are the folder's, not a list

`environmentsForRole` names `dev` and `staging` because two environment variables on the
deployment say so. A team whose `prod` folder exists and whose developers may read it had
no way to receive `.env.prod`, and a team with no `staging` had to set
`INFISICAL_STAGING_ENVIRONMENT=` to stop every run failing against an environment
Infisical does not have. Both are the same bug: a list maintained in a second place from
the thing it describes.

So riabuild-web asks Infisical which environments the project has, and which of them
contain the repository's folders, and that answer is the list. Folders present in `dev` and
`prod` and absent from `staging` produce `.env.dev` and `.env.prod`, in that order, and no
`.env.staging` — which is what the team's Infisical project already says, expressed
somewhere a laptop can read it.

**All of them, not any of them.** A repository mapped to two folders counts an environment
only when that environment has both, because the export is a fold and a missing folder is
a 404 that fails the whole pull. Half a `.env.dev` that starts nothing is worse than no
`.env.dev` and a line saying which environment was skipped.

**One role distinction survives, and only one.** A candidate gets the base environment
(`INFISICAL_ENVIRONMENT`, `dev` by default) and nothing else. That is not a second copy of
Infisical's RBAC — it is the same narrowing `identityForRole` already makes, kept because
a candidate is brokered through `mi-candidate` and naming `prod` on their behalf buys
nothing but an Infisical denial their developer cannot act on. A developer or a lead gets
the folder's environments whole, which is the half that was wrong.

Discovery reads **folder names**, never a secret value. The invariant in
`riabuild-web/AGENTS.md` is unchanged and was written for exactly this distinction:
riabuild-web performs universal-auth login and hands the CLI a token, and the CLI fetches
the payload.

### Where each half of the answer is read

The same split `secretEnvironments` already lives under, for the same stated reason:

| Caller | Endpoint | Why not the other one |
|---|---|---|
| `env_local::check()` | `GET /api/v1/secrets/scope?repo=…` | runs on every `riabuild --check`; must not broker a credential and write an audit row to learn which files ought to exist |
| `env_local::apply()` | `POST /api/v1/secrets/token` (with `repo`) | the credential and the scope it was minted for arrive together, so the two can never describe different folders |
| `riabuild internal infisical` | `POST /api/v1/secrets/token` (with `repo`) | the shim fills in `--path` and `--env` for the repository the developer is standing in |

`GET /api/v1/secrets/scope` is new and carries no credential — a path, a list of
environment names, and when the row last changed. It is the cheap question, and it is the
one asked on every run.

### A path change is staleness

`.env.dev` written from `/apps/hub` is wrong the moment the row says `/apps/payments`, and
nothing on disk can tell — and so is one written before a second folder was added to the
list. The row carries `updatedAt`, `check()` compares it against the
file's mtime exactly as it already compares `secretsUpdatedAt`, and the refill happens on
the next run. This is why the mapping is a table with its own timestamp rather than a blob
on `orgConfig`: an org-wide "secrets changed" clock would restage every repository's files
whenever any one row moved.

## Compatibility, and the day this deploys

**Old CLIs are unchanged.** `repo` is an optional field on `POST /api/v1/secrets/token`;
a request without one gets `INFISICAL_SECRET_PATH` and `environmentsForRole` exactly as
today, and `GET /api/v1/org/config` goes on carrying `secretEnvironments`. A CLI released
before this reads neither new thing and is not stranded — the rule in
`.agents/skills/riabuild-api/SKILL.md` about required fields, applied.

**New CLIs against an old deployment** get a 404 from `/secrets/scope`, which
`env_local::check()` reads as "this deployment predates per-repository paths" and falls
back to `org.secret_environments`. The fallback is named and temporary; it is not a
silent guess, because a silent guess here fills a checkout from the wrong folder.

**The migration is the part that would otherwise strand everybody.** "No row means no
secrets" applied to a deployment that has never had a row means every developer loses
their `.env.dev` on the day this ships, and finds out at once. So there is a named
migration, in the shape `denyEveryDotenvFile` set:

```sh
npx convex run secretPaths:seedFromDeploymentPath --prod
```

It writes one row — `config.repoSlug` → `secretPaths()`, the deployment's own list, in the
deployment's own order — and only when the table is empty. An org that has already mapped anything keeps its choices, and re-running it
does nothing. `INFISICAL_SECRET_PATH` stays read for old CLIs and for that migration, and
for nothing else.

## Surfaces

| Surface | Change |
|---|---|
| `convex/schema.ts` | `repoSecretPaths` — `repoSlug`, `secretPaths`, `updatedAt`, `updatedBy`; indexed by slug |
| `convex/secretPaths.ts` | new — `list`, `set`, `remove` for the dashboard; `forRepo` for the endpoints; `seedFromDeploymentPath` |
| `convex/infisical.ts` | `discoverEnvironments` — the project's environments, filtered to the ones holding every one of the repository's folders, narrowed for a candidate |
| `convex/http.ts` | `GET /api/v1/secrets/scope`; `POST /api/v1/secrets/token` takes an optional `repo` |
| `src/components/SecretPaths.tsx` | the table and its form, `DataTable` + `Field`, the `SharedServers` shape |
| `src/data/` | `secretPaths` on the context, the three mutations, a fixture state |
| `crates/api/src/secrets.rs` | `SecretScope`; `broker_for(repo)`; `scope_for(repo)` |
| `crates/tasks/src/env_local/` | `check()` and `apply()` read the run's repository; an unmapped repository is satisfied and says so |
| `crates/cli/src/internal/infisical.rs` | the shim's `--path`/`--env` come from the active repository |
| docs | this spec; the picker spec's "out of scope" line; both `AGENTS.md` |

## What it costs, said out loud

- **A lead types a repository slug, and the server stores it unvalidated against GitHub.**
  It has to: the architecture rule is that riabuild holds no permission logic about which
  repositories exist, because the CLI asks GitHub through the developer's own `gh`. So the
  dashboard checks the *shape* — `Repo::parse`'s rules, restated — and nothing else. A row
  naming a repository nobody has is inert: no run is ever about it.
- **Discovery is upstream calls on a read path.** A login, a project fetch, and one folder
  listing per environment. It is cached per path for five minutes, which is short enough
  that a folder created in Infisical shows up in the same coffee break and long enough
  that a team provisioning together does not make the same four calls each.
- **A folder a developer can list but cannot read still produces a file riabuild fails to
  fill.** Discovery asks the same machine identity the credential will be minted from, so
  the two agree in every case we can construct; where Infisical splits those permissions
  apart, the failure is the one `env_local` already reports and names the environment.
- **The environments column is a live read in a dashboard.** A lead whose Infisical is
  down sees the paths and `—` where the environments go, not an error screen: the table's
  job is editing paths, and it keeps doing that job when the thing it annotates is
  unreachable.

## Out of scope

No per-repository *identities* — which machine identity brokers a credential is still the
member's role, administered in Infisical. No per-repository Claude settings, version
floors, or anything else on `orgConfig`; this is one column of one table, not the start of
a per-repository config surface. No secret values in riabuild-web, in either direction.
