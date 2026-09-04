# Deploying riabuild

## Current state

| Piece | Status |
|---|---|
| Convex production | **live** — `handsome-vulture-127.eu-west-1.convex.cloud`, HTTP actions on `…convex.site` |
| Convex dev | **live** — `wary-bandicoot-285.eu-west-1.convex.cloud` |
| Convex project | `lowerkinded / riabuild` |
| `/api/v1` | **live**, returning 401 to anonymous callers |
| Secret brokering | **working** — verified end to end through Convex's network |
| Infisical | `https://infisical.clubria.com`, project `AI Builders`, paths `/tenant/aibuilders/frontend` and `/tenant/aibuilders/convex` |
| Dashboard | **live** — <https://riabuild.clubria.com> (Cloudflare Pages, `riabuild-web`) |
| GitHub sign-in | configured — verify the OAuth callback URL matches §2 |
| Org membership checks | working — `GITHUB_ORG_TOKEN` verified against the live org |
| DNS | `riabuild.clubria.com` CNAME → `riabuild-web.pages.dev`, DNS-only |

To reproduce the local `.env.local` for this project:

```
CONVEX_DEPLOYMENT=dev:wary-bandicoot-285
VITE_CONVEX_URL=https://wary-bandicoot-285.eu-west-1.convex.cloud
```

The rest of this document is the remaining work. Everything left needs a credential the
repository does not contain.

## 1. Convex deployment

Already done for this project. To repeat elsewhere — and note `--login-flow paste`,
which is what makes this work over SSH, since the default loopback flow needs a browser
on the same machine:

```sh
cd riabuild-web
npx convex login --login-flow paste --no-open   # prints a URL; paste the token back
npx convex dev --once --configure new --team <team> --project riabuild --dev-deployment cloud
npx convex deploy -y                            # creates the production deployment
```

`convex deploy` prints two hostnames:

| Hostname | Serves |
|---|---|
| `<deployment>.convex.cloud` | the client API the dashboard uses |
| `<deployment>.convex.site`  | HTTP actions — this is where `/api/v1` lives |

## 2. Deployment environment variables

```sh
npx convex env set AUTH_GITHUB_ID       <oauth app client id>
npx convex env set AUTH_GITHUB_SECRET   <oauth app client secret>
npx convex env set SITE_URL             https://riabuild.clubria.com
npx convex env set RIABUILD_BOOTSTRAP_LEADS "ilya,<other lead logins>"
npx convex env set RIABUILD_GITHUB_ORG  Clubria
npx convex env set GITHUB_ORG_TOKEN     <PAT with read:org, held by a Clubria member>
npx convex env set INFISICAL_PROJECT_ID <project id>
npx convex env set INFISICAL_ENVIRONMENT dev
# Optional; defaults to `staging`. Developers and leads get this environment as
# `.env.staging` beside `.env.dev`; candidates get `.env.dev` alone. Set it to an
# empty string if the Infisical project has no staging environment.
npx convex env set INFISICAL_STAGING_ENVIRONMENT staging
npx convex env set INFISICAL_CANDIDATE_CLIENT_ID     <mi-candidate client id>
npx convex env set INFISICAL_CANDIDATE_CLIENT_SECRET <mi-candidate client secret>
npx convex env set INFISICAL_DEVELOPER_CLIENT_ID     <mi-developer client id>
npx convex env set INFISICAL_DEVELOPER_CLIENT_SECRET <mi-developer client secret>
# Optional, and the credential that may do everything in the project — see
# section 3. Unset, a lead is brokered through `mi-developer`, which is where
# every lead was before `mi-lead` existed.
npx convex env set INFISICAL_LEAD_CLIENT_ID          <mi-lead client id>
npx convex env set INFISICAL_LEAD_CLIENT_SECRET      <mi-lead client secret>
```

Never set `RIABUILD_DEV_SEED` on a production deployment. It gates `convex/devSeed.ts`,
which exists only for end-to-end tests against a local backend.

**Without `GITHUB_ORG_TOKEN` every secret-brokering request returns 503
`org_check_unavailable`.** That is deliberate: riabuild fails closed, and says it could
not check rather than claiming the developer was removed from the org.

The same token also authenticates the release check in `convex/release.ts`, which reads a
public endpoint and needs no scope for it. It is sent for the rate limit: unauthenticated
calls get sixty an hour **per source IP**, and that IP is Convex's, shared with every
other deployment on it. Releasing while that shared budget is spent means the release
publishes and nobody is offered it — see `releasing.md`.

### The GitHub OAuth app

Create it at <https://github.com/organizations/Clubria/settings/applications>.

- Homepage URL: `https://riabuild.clubria.com`
- Authorization callback URL: `https://<deployment>.convex.site/api/auth/callback/github`

The provider requests `read:user user:email read:org`. `read:org` is not optional — the
sign-in gate and the profile prefill both depend on it.

## 3. Infisical machine identities

Service tokens and API keys were deprecated in April 2024. Three **machine identities**
with universal auth:

| Identity | Access |
|---|---|
| `mi-candidate` | the subset of dev paths a candidate may read |
| `mi-developer` | all dev paths, in both `dev` and `staging` |
| `mi-lead` | everything the project has |

Path scoping is enforced by Infisical's own RBAC. riabuild only chooses which identity
to authenticate as, and never sees a secret value.

### `mi-lead` is the one with no scoping

Create it with **full access to the project** — the built-in admin role over every
subject, not a hand-listed subset: reading *and writing* secrets, creating, renaming and
deleting the folders they live in, secret imports and rollbacks, certificate management
and its authorities, environments, tags, webhooks, the audit log, and whatever Infisical
adds next. A lead administers the team's secrets; a permission there they do not have is
a lead going round riabuild to do their job, which is the outcome this exists to prevent.

Say the cost out loud rather than discovering it later: **a lead's laptop can, for the
five minutes that token lives, delete the team's secrets.** What bounds it is the same
three things that bound every other brokered credential — the token is short-lived and
never written down, the request re-verifies GitHub org membership before minting it, and
`auditLog` records who asked. Infisical's own audit log records what they then did with
it, and it is the only record of that: riabuild brokers the credential and never sees a
call made with it.

Grant it in Infisical, never here. riabuild names three credentials and no permissions,
so widening or narrowing a lead is a change an admin makes to the identity — one that
takes effect on the next brokered token, everywhere, with no riabuild release involved.
A permission list living in this repository would be riabuild deciding what a laptop may
do to the team's secrets, which is the boundary in `../CLAUDE.md` seen from the
authorization side.

`INFISICAL_LEAD_CLIENT_ID` and `INFISICAL_LEAD_CLIENT_SECRET` are **optional**, and a
deployment that sets neither brokers its leads through `mi-developer` exactly as it did
before this identity existed. That is the only fallback: a half-set pair — an id typed
and the secret still to come — is treated as unset rather than authenticated with, since
an incomplete pair buys a 401 from Infisical instead of a working developer credential.

### Environments

riabuild asks for `dev` and `staging` on behalf of a developer or a lead, and for `dev`
alone on behalf of a candidate — one `.env.<environment>` per environment, in the
checkout. That list is unchanged by `mi-lead`: what a lead's credential *may* reach is
now the whole project, but what riabuild pulls into a checkout on their behalf is still
the two environments every developer gets. A lead reaching further does it by hand —
`infisical secrets --env=prod` through the shim — which is a command they typed rather
than a file riabuild wrote.

`mi-developer` needs **every** folder in `INFISICAL_SECRET_PATH` readable in the
`staging` environment as well; if one is not, a developer's run fails on the staging
export rather than degrading quietly. An org with no staging environment sets
`INFISICAL_STAGING_ENVIRONMENT` to the empty string, and every role gets `dev` alone.

`INFISICAL_SITE_URL` must be set — the code otherwise defaults to `app.infisical.com`,
where these identities do not exist and the login returns 401. It is
`https://infisical.clubria.com`. Convex's servers can reach it; that was verified rather
than assumed, since an instance reachable from a developer laptop is not necessarily
reachable from eu-west-1. Moving instances moves `INFISICAL_PROJECT_ID` and all three
identity pairs with it — they are per instance, and a client id from the old one
authenticates against the new one exactly as well as a wrong password does.

**Read that hostname twice.** It is the service under our domain —
`infisical.clubria.com` — and not a tenancy of Infisical's own SaaS, which is what
`clubria.infisical.com` would be. Both spellings are plausible English and only one has a
DNS record. The wrong one was set on production on 2026-09-04 and every developer's pull
failed the same way for hours: `POST /api/v1/secrets/token` answered `upstream_error`, the
CLI reported `riabuild could not broker an Infisical credential`, and **nothing anywhere
said "that host does not exist"** — a name that fails to resolve and an instance that
refuses a login are one error message here. The check that names it in one line, before
touching anything else:

```sh
curl -sS https://$(npx convex env get INFISICAL_SITE_URL --prod | sed 's|https\?://||')/api/status
```

`{"message":"Ok",…}` means the host is right and the failure is the credential; a DNS
error means it is the host. → `ai-builders-hub/docs/infra-domains-clubria-move.md`, which
is the authority for every one of these subdomains.

### Deploying the dev/staging split onto an existing deployment

Two steps, and the second is not optional on a deployment that has ever saved org config:

```sh
npx convex env set INFISICAL_STAGING_ENVIRONMENT staging
npx convex run org:denyEveryDotenvFile --prod
```

The migration adds `Read(./.env.*)` to the org's Claude Code deny list. Without it,
`Read(./.env)` — an exact path — covers neither `.env.dev` nor `.env.staging`, and the
secrets riabuild has just brokered are readable by every Claude Code account.
`org:backfillClaudeDefaults` does **not** cover this: it fills keys that are absent, and
`permissions.deny` is present on every stored row, so it reports success and changes
nothing. See "Changing `DEFAULT_CLAUDE_SETTINGS` reaches nobody on its own" in
`../riabuild-web/CLAUDE.md`.

### Deploying the opus/sonnet split onto an existing deployment

One step, and it is not optional on a deployment that has ever saved org config:

```sh
npx convex run org:backfillClaudeDefaults --prod
```

The org's Claude Code settings now carry `model: "opus"` and
`env.CLAUDE_CODE_SUBAGENT_MODEL: "sonnet"` — a session on Opus whose subagents default to
Sonnet. Unlike the deny-list migration above, the ordinary backfill **is** the right tool
here: `model` is absent at the top level of every stored row, and `env` is present on all
of them, so the variable arrives by `fillMissing` descending into it. An org that already
answered either key keeps its answer.

Without this the edit reaches fresh deployments and nowhere else, and the symptom is not
an error: every session simply goes on using whatever model it defaulted to, with nothing
on any screen naming one.

### `INFISICAL_SECRET_PATH` is not `/`, and is two folders

The `dev` environment has **no secrets at its root** — they live in folders, and
`infisical export` in this CLI version has no recursive flag, only `--path`. Left at the
default `/`, brokering succeeds and exports **zero** secrets, which surfaces as
riabuild's "Infisical returned no secrets" failure rather than anything mentioning
paths.

It is set to two of them, comma-separated:

```sh
npx convex env set INFISICAL_SECRET_PATH \
  /tenant/aibuilders/frontend,/tenant/aibuilders/convex
```

riabuild exports each in the order they are named and merges the results into one
`.env.<environment>`, **later winning** a key both folders hold — which is dotenv's own
rule and the same one `dev-env/pull-dev-env.sh` in the checkout applies. So the
credential folder goes last. `secretPath`, the single folder the API still returns, is
that last entry: it is what a bare `infisical export` through the shim defaults to, and
all that a riabuild released before this can read.

Why two. On **2026-08-29** AI Builders moved out of the root-level folders into a tenant
tree — *the environment picks the tier, the path picks the tenant* — and on 2026-08-30
the old ones were deleted, so `/dev-env` is not stale, it is a **404**:

| Path | Holds |
|---|---|
| `/tenant/aibuilders/frontend` | every `VITE_*` the image build bakes in |
| `/tenant/aibuilders/convex` | `CONVEX_SELF_HOSTED_URL` + `_ADMIN_KEY`, the stack's own vars, and the developer-only secrets |
| `/tenant/aibuilders/convex-runtime` | what `deploy-convex.mjs` pushes into the deployment — **not** part of a developer's env file |

A developer needs the first two: either half alone writes a `.env.dev` that does not
start the app, and the failure lands later, at `pnpm run dev`, rather than here. Both
paths are the same in `dev` and in `staging` — the flagship has no `prod` environment —
so `mi-developer` needs both readable in `staging` too. The layout is
`ai-builders-hub`'s `docs/secrets-storage.md#tenant-infisical-layout`, and
`scripts/lib/envTier.mjs` there is its registry; a `pnpm run guards` in that repository
fails a commit that names a retired path.

### Shorten the access token TTL

The brokered token currently lives **30 days** (`expiresIn: 2592000`), which is the
identity default. The design calls for a short-lived credential, and riabuild pipes it
straight into `infisical export` and discards it — nothing needs it for more than a few
seconds. Set **Access Token TTL to 300** on all three identities in Infisical; nothing in
riabuild has to change.

## 4. Hosting the dashboard

> **The two-account trap.** This Cloudflare login has two accounts:
> `Aibuildersclub@proton.me` (`185f8d17…`) owns the **clubria.com zone**, and
> `Lowerkinded@gmail.com` (`2570e0b4…`) is where **Workers and Pages** permissions
> live. Calling the wrong one returns `Authentication error [code: 10000]`, which
> reads exactly like a missing permission and is not. Always set
> `CLOUDFLARE_ACCOUNT_ID=2570e0b4e3586a4da93eabe5d530f27d` when deploying.
>
> This split is also why the dashboard is on **Pages, not Workers**: a Workers
> custom domain requires the zone and the Worker in the same account, whereas
> Pages can serve a custom domain whose zone lives elsewhere.

**Automated.** `.github/workflows/deploy.yml` publishes `riabuild.clubria.com` on every
commit to main that touches `riabuild-web/`, after re-running lint and tests. It needs
`CLOUDFLARE_API_TOKEN` as a repository secret; the account id and `VITE_CONVEX_URL` are
public and defaulted in the workflow, overridable as repository *variables*.

The path filter is load-bearing: `release.yml` commits `Formula/riabuild.rb` to main on
every CLI release, and without it each release would redeploy an unchanged dashboard.

> **Convex is deployed by the same workflow, but only if `CONVEX_DEPLOY_KEY` is set**
> (Convex dashboard → Settings → Deploy keys → `gh secret set CONVEX_DEPLOY_KEY`).
> Without it the job still publishes the dashboard and emits a warning, so the static
> site can be newer than the functions it calls. Backend runs first when the key is
> present, so a deploy never leaves the dashboard calling functions that do not exist.

To deploy by hand — the same command the workflow runs:

```sh
cd riabuild-web
export CLOUDFLARE_API_TOKEN=…
export CLOUDFLARE_ACCOUNT_ID=2570e0b4e3586a4da93eabe5d530f27d
VITE_CONVEX_URL=https://handsome-vulture-127.eu-west-1.convex.cloud pnpm deploy:web
```

`public/_redirects` is load-bearing: it serves `index.html` for unmatched paths, so
`/cli` works on a cold load — which is exactly how a developer reaches it, by typing the
URL riabuild printed in a terminal.

### The old Workers route, for reference

Not used. A static-asset Worker would work, but only in the account that owns the
zone — see the trap above.

## 5. DNS

Only **one** record is needed, and it already exists:

| Name | Type | Target | Proxy |
|---|---|---|---|
| `riabuild.clubria.com` | CNAME | `riabuild-web.pages.dev` | **DNS only** |

It must stay unproxied. The zone is in a different account from the Pages project, so
Cloudflare validates the custom domain and issues its certificate over the direct
connection; orange-clouding it breaks that validation.

### There is no `api.riabuild.clubria.com`, and it cannot be added for free

Both routings were tried against the live zone and both fail TLS:

- **Unproxied** — TLS terminates at Convex, which holds a certificate for `*.convex.site`
  only. `curl` reports `ssl_verify_result=1`.
- **Proxied** — Cloudflare's Universal SSL covers `clubria.com` and `*.clubria.com`, one
  label below the apex. `api.riabuild` is two labels, so no edge certificate exists.

Fixing it properly needs Convex's custom-domain feature (a paid plan), or Advanced
Certificate Manager plus an origin rule to rewrite the SNI. Neither is worth it: no
developer ever types this hostname. The CLI's `DEFAULT_API_URL` therefore points straight
at `handsome-vulture-127.eu-west-1.convex.site`, which has a valid certificate today.

If you later add a Convex custom domain, change that constant and cut a release — it is
one line in `riabuild-cli/src/api/mod.rs`, and `RIABUILD_API_URL` overrides it meanwhile.

A single-label name like `riabuild-api.clubria.com` would get an edge certificate, but
Convex routes HTTP actions by hostname and would not recognise it, so it still needs the
custom-domain feature.

## 6. Homebrew tap

Fully automated, and unlike everything else on this page it needs no credential you
do not already have. `.github/workflows/release.yml` builds both macOS architectures
on a `v<version>` tag, publishes them as a GitHub release, and commits the rendered
formula to `Formula/riabuild.rb` on main. **`docs/releasing.md` is the runbook.**

The tap is `Clubria/homebrew-tap`, a repository holding only the formula, named
exactly what Homebrew guesses from the tap name `clubria/tap` so that install needs
no `brew tap` line:

```sh
brew install clubria/tap/riabuild
```

That is a second repository, so it needs a credential the workflow's own
`GITHUB_TOKEN` is not: set **`HOMEBREW_TAP_TOKEN`** on this repository to a
fine-grained PAT with `contents: write` on `Clubria/homebrew-tap` and no other
permission or repository. The release workflow fails the `formula` job when it is
missing, rather than publishing a release the tap never learns about.

Both repositories and the release assets must stay **public**: `brew` clones and
fetches with no credentials. The binary holds no secrets, and every gate is
re-verified server-side, so this costs nothing.

After each release, set the version fields from the dashboard's lead panel. Both are
release dates (`2026.08.04`), not semver:

- `latestCliVersion` — what the startup check offers to upgrade to
- `minCliVersion` — the floor below which the CLI refuses to run

Neither is derived from GitHub, so a published release reaches nobody until
`latestCliVersion` names it. `minCliVersion` hard-blocks people mid-workday. Raise it
deliberately.

## 7. Migrating a schema field from optional to required

Applies whenever a schema change promotes a field from `v.optional(...)` to required on
a table that already has documents in production — the current case is
`members.memberId`, going from `v.optional(v.string())` to `v.string()`.

**Why the order matters.** Convex validates every existing document against a table's
schema at deploy time. Deploying a schema where a field is required is rejected outright
if even one production document is missing that field — there is no partial or lazy
migration; a required field either matches every row already there, or the deploy does
not go through at all.

**The required order, and why each step has to finish before the next starts:**

1. **Deploy with the field still optional**, alongside the code that mints it going
   forward (new rows get it from application code) and a one-shot internal mutation that
   backfills old rows — `members.backfillMemberIds` is that mutation for `memberId`. At
   this point new rows have the field; rows written before this deploy may not.
2. **Run the backfill against production**: `npx convex run members:backfillMemberIds
   --prod`. It patches only the rows still missing the field and returns how many it
   changed — 0 means every row already has one, which is what you want to see, not a
   sign nothing happened. It is idempotent: running it again after it has already
   finished is safe and returns 0.
3. **Deploy again with the field now required.** This is the deploy that actually turns
   the constraint on; only run it once step 2's count is verified back to 0 on a repeat
   run, meaning nothing is left to backfill.

**This matters more than a normal runbook step because deploys are automatic.**
`.github/workflows/deploy.yml` runs `npx convex deploy -y` on every push to `main` that
touches `riabuild-web/**`, with no manual approval gate. Whoever merges a change like
this must make sure step 2 has already happened against production *before* the commit
that flips the field to required reaches `main` — if a single merge carries both the
"add the optional field and the backfill" commit and the "make it required" commit at
once, the first and only automatic deploy that follows applies the required schema
straight against a production table where nothing has been backfilled yet, and it fails
as described below. (Landing the optional-field commit and the required-field commit as
two separate merges to `main`, with the backfill run by hand in between, makes this
per-push automation enforce the order on its own instead of relying on whoever merges
reading this section first.)

**What a failed deploy looks like.** The `riabuild.clubria.com` job's "Deploy Convex
functions" step runs `npx convex deploy -y`, which refuses to push a schema any existing
document violates and exits non-zero, naming the table and the field it could not
validate (an error to the effect of a document in `members` not matching the schema
because `memberId` is missing). That step failing fails the whole job before the later
"Build and deploy to Cloudflare Pages" step ever runs, so **neither the backend nor the
dashboard update** — production keeps serving whatever was live before the push, and the
only visible symptom is a red `riabuild.clubria.com` run under the `deploy-web`
concurrency group, with a Convex schema-validation message in the "Deploy Convex
functions" step's log rather than a network or auth error. It is not stuck forever: the
concurrency group only blocks runs that are actively in progress, so the next push that
touches `riabuild-web/**` still triggers and runs normally.

**To recover:** run the backfill mutation against `--prod` (step 2 above) — Convex
production access is required for this, same as anywhere else in this document — confirm
a repeat run returns 0, then re-run the failed workflow (or push again) so the required
schema deploy is retried.

## Verifying a deployment

```sh
curl -s https://handsome-vulture-127.eu-west-1.convex.site/api/v1/me
# expect 401 unauthenticated — the endpoint is live and refusing anonymous callers
```

Then sign in on the dashboard, run `riabuild` on a machine, and confirm the session
appears under "Your machines".
