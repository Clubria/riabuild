# Deploying riabuild

## Current state

| Piece | Status |
|---|---|
| Convex production | **live** — `handsome-vulture-127.eu-west-1.convex.cloud`, HTTP actions on `…convex.site` |
| Convex dev | **live** — `wary-bandicoot-285.eu-west-1.convex.cloud` |
| Convex project | `lowerkinded / riabuild` |
| `/api/v1` | **live**, returning 401 to anonymous callers |
| GitHub sign-in | **not working** — `AUTH_GITHUB_ID` / `AUTH_GITHUB_SECRET` unset |
| Org membership checks | **not working** — `GITHUB_ORG_TOKEN` unset, so brokering returns 503 |
| Secret brokering | **not working** — Infisical identities unset |
| Dashboard hosting | **not deployed** — needs a Cloudflare token with Workers permission |
| DNS | no `riabuild` records; see the TLS note in §5 |

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

`wrangler.jsonc` configures a static-asset Worker. `not_found_handling:
single-page-application` is load-bearing — the dashboard routes on `pathname`, so
`/cli/authorize` must serve `index.html` when opened directly, which is exactly how the
CLI opens it.

```sh
cd riabuild-web
VITE_CONVEX_URL=https://handsome-vulture-127.eu-west-1.convex.cloud pnpm build
CLOUDFLARE_API_TOKEN=… npx wrangler deploy
```

**The API token needs `Account → Workers Scripts → Edit`.** A token made from the "Edit
zone DNS" template can create DNS records but returns `Authentication error [code:
10000]` here. Verify with:

```sh
curl -s "https://api.cloudflare.com/client/v4/accounts/185f8d1747f1e766b83b40ddebdbfafa/workers/scripts" \
  -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN"
```

## 5. DNS

Only **one** record is needed:

| Name | Target | Why |
|---|---|---|
| `riabuild.clubria.com` | the Worker | what developers visit |

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

The CLI ships as `clubria/tap/riabuild`. Build release binaries for `darwin-arm64` and
`darwin-x64`, publish them as a GitHub release, and point a formula in the
`clubria/homebrew-tap` repository at the tarballs.

After publishing, set the version fields from the dashboard's lead panel:

- `latestCliVersion` — what the startup check offers to upgrade to
- `minCliVersion` — the floor below which the CLI refuses to run

`minCliVersion` hard-blocks people mid-workday. Raise it deliberately.

## Verifying a deployment

```sh
curl -s https://handsome-vulture-127.eu-west-1.convex.site/api/v1/me
# expect 401 unauthenticated — the endpoint is live and refusing anonymous callers
```

Then sign in on the dashboard, run `riabuild` on a machine, and confirm the session
appears under "Your machines".
