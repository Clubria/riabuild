# Deploying riabuild

Everything in this document needs a credential the repository does not contain. The
code is complete and verified against a local Convex backend; these are the steps that
put it on `riabuild.clubria.com`.

## 1. Convex deployment

```sh
cd riabuild-web
npx convex login          # interactive; opens a browser
npx convex deploy         # creates the production deployment
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
npx convex env set INFISICAL_CANDIDATE_CLIENT_ID     <mi-candidate client id>
npx convex env set INFISICAL_CANDIDATE_CLIENT_SECRET <mi-candidate client secret>
npx convex env set INFISICAL_DEVELOPER_CLIENT_ID     <mi-developer client id>
npx convex env set INFISICAL_DEVELOPER_CLIENT_SECRET <mi-developer client secret>
```

Never set `RIABUILD_DEV_SEED` on a production deployment. It gates `convex/devSeed.ts`,
which exists only for end-to-end tests against a local backend.

**Without `GITHUB_ORG_TOKEN` every secret-brokering request returns 503
`org_check_unavailable`.** That is deliberate: riabuild fails closed, and says it could
not check rather than claiming the developer was removed from the org.

### The GitHub OAuth app

Create it at <https://github.com/organizations/Clubria/settings/applications>.

- Homepage URL: `https://riabuild.clubria.com`
- Authorization callback URL: `https://<deployment>.convex.site/api/auth/callback/github`

The provider requests `read:user user:email read:org`. `read:org` is not optional — the
sign-in gate and the profile prefill both depend on it.

## 3. Infisical machine identities

Service tokens and API keys were deprecated in April 2024. Create two **machine
identities** with universal auth:

| Identity | Access |
|---|---|
| `mi-candidate` | the subset of dev paths a candidate may read |
| `mi-developer` | all dev paths |

Path scoping is enforced by Infisical's own RBAC. riabuild only chooses which identity
to authenticate as, and never sees a secret value.

## 4. Hosting the dashboard

`pnpm build` produces a static `dist/`. Any static host works; Cloudflare Pages keeps it
on the same account as the DNS:

```sh
cd riabuild-web
pnpm build
cf deploy            # needs `cf auth login` or CLOUDFLARE_API_TOKEN
```

Set `VITE_CONVEX_URL` to the `.convex.cloud` hostname at build time.

## 5. DNS

Two names, both on the `clubria.com` zone:

| Name | Target | Why |
|---|---|---|
| `riabuild.clubria.com` | the static dashboard host | what developers visit |
| `api.riabuild.clubria.com` | `<deployment>.convex.site` | what the CLI calls |

```sh
cf dns records create --zone clubria.com \
  --type CNAME --name api.riabuild --content <deployment>.convex.site --proxied false
```

Leave the API record unproxied: it is a JSON API for a Rust client, and Cloudflare's
proxy adds nothing but a failure mode.

The CLI's defaults are compiled in (`api/mod.rs`), so nothing needs configuring on a
developer's machine. Both are overridable with `RIABUILD_WEB_URL` and `RIABUILD_API_URL`
for local development.

## 6. Homebrew tap

The CLI ships as `clubria/tap/riabuild`. Build release binaries for `darwin-arm64` and
`darwin-x64`, publish them as a GitHub release, and point a formula in the
`clubria/homebrew-tap` repository at the tarballs.

After publishing, set the version fields from the dashboard's lead panel:

- `latestCliVersion` — what the startup check offers to upgrade to
- `minCliVersion` — the floor below which the CLI refuses to run

`minCliVersion` hard-blocks people mid-workday. Raise it deliberately.

## Verifying a deployment

```sh
curl -s https://api.riabuild.clubria.com/api/v1/me
# expect 401 unauthenticated — the endpoint is live and refusing anonymous callers
```

Then sign in on the dashboard, run `riabuild` on a machine, and confirm the session
appears under "Your machines".
