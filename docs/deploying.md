# Deploying riabuild

## Current state

| Piece | Status |
|---|---|
| Convex production | **live** — `handsome-vulture-127.eu-west-1.convex.cloud`, HTTP actions on `…convex.site` |
| Convex dev | **live** — `wary-bandicoot-285.eu-west-1.convex.cloud` |
| Convex project | `lowerkinded / riabuild` |
| `/api/v1` | **live**, returning 401 to anonymous callers |
| Secret brokering | **not working** — Infisical identities unset, so `/secrets/token` returns 503 |
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

```sh
cd riabuild-web
export CLOUDFLARE_API_TOKEN=…
export CLOUDFLARE_ACCOUNT_ID=2570e0b4e3586a4da93eabe5d530f27d
VITE_CONVEX_URL=https://handsome-vulture-127.eu-west-1.convex.cloud pnpm deploy:web
```

`public/_redirects` is load-bearing: it serves `index.html` for unmatched paths, so
`/cli/authorize` works on a cold load — which is exactly how the CLI opens it.

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
