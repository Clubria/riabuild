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
pass "seeded @$E2E_LOGIN as a developer, org pointed at $E2E_REPO_SLUG"

# The seeded session has to survive the real authentication path before the CLI
# is asked to depend on it, or a bad seed reads as a broken CLI.
ME="$(curl -s -H "authorization: Bearer $SESSION_TOKEN" "$API_URL/api/v1/me")"
check_contains "the seeded session authenticates against /api/v1/me" "$ME" "\"githubLogin\":\"$E2E_LOGIN\""

