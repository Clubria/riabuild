# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

# ---------------------------------------------------------------------------
# 7. Seed
# ---------------------------------------------------------------------------

step "Seed"

run_convex() { CONVEX_AGENT_MODE=anonymous npx convex run "$1" "$2" >/dev/null; }

# The token is minted here and only its hash is ever sent. A fixture that
# inserted a raw token would be testing a system that does not exist — every
# real session is looked up by SHA-256 of the bearer token.
SESSION_TOKEN="rb_e2e_$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
TOKEN_HASH="$(printf '%s' "$SESSION_TOKEN" | shasum -a 256 | cut -d' ' -f1)"

run_convex devSeed:seedForE2e \
  "{\"githubLogin\":\"$E2E_LOGIN\",\"tokenHash\":\"$TOKEN_HASH\",\"role\":\"developer\"}"
run_convex devSeed:seedOrgConfigForE2e "$(python3 - "$E2E_REPO_SLUG" "$E2E_CLAUDE_SETTINGS" \
  "$E2E_MIN_CLI_VERSION" "$E2E_LATEST_CLI_VERSION" <<'PY'
import json, sys
slug, settings, minimum, latest = sys.argv[1:5]
print(json.dumps({
    "repoSlug": slug,
    "claudeSettings": settings,
    "minCliVersion": minimum,
    "latestCliVersion": latest,
}))
PY
)"

# The same migration `docs/deploying.md` tells a maintainer to run, run the
# same way. A repository with no row in `repoSecretPaths` gets no `.env` files
# at all — that is the feature — so a throwaway backend that skipped this would
# fail stage 11 on the missing `.env.dev`, and seeding the row by hand instead
# would leave the one command production depends on untested.
run_convex secretPaths:seedFromDeploymentPath '{}'
pass "seeded @$E2E_LOGIN as a developer, org pointed at $E2E_REPO_SLUG"

# The seeded session has to survive the real authentication path before the CLI
# is asked to depend on it, or a bad seed reads as a broken CLI.
#
# `api_curl` carries the version header. The floor is E2E_MIN_CLI_VERSION as of
# the seed two lines up, and `guard()` enforces it ahead of the session, so a
# bare `curl` here would be turned away 409 without the token ever being looked
# at — and this assertion would blame the seed for it.
ME="$(api_curl -H "authorization: Bearer $SESSION_TOKEN" "$API_URL/api/v1/me")"
check_contains "the seeded session authenticates against /api/v1/me" "$ME" "\"githubLogin\":\"$E2E_LOGIN\""

# The migration's own answer goes to /dev/null with every other seed's, so ask
# the endpoint the CLI will ask instead. `configured:true` is the difference
# between a run that
# writes `.env.dev` and one that correctly writes nothing at all — and the
# second failure would surface four stages later as a missing file.
SCOPE="$(api_curl -H "authorization: Bearer $SESSION_TOKEN" \
  "$API_URL/api/v1/secrets/scope?repo=$(python3 -c 'import sys,urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "$E2E_REPO_SLUG")")"
check_contains "$E2E_REPO_SLUG takes its secrets from the deployment's folders" \
  "$SCOPE" '"configured":true'


