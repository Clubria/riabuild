# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

# ---------------------------------------------------------------------------
# 6. Deployment configuration
# ---------------------------------------------------------------------------

step "Deployment configuration"

set_env() { CONVEX_AGENT_MODE=anonymous npx convex env set "$1" "$2" >/dev/null; }

# Note what is deliberately *not* set: RIABUILD_DEV_AUTH. It would make the
# dashboard's membership check return `member` unconditionally. The /api/v1 org
# re-verification never consults it, and this run wants the real check on both
# sides, so leaving it unset is the point rather than an oversight.
set_env RIABUILD_DEV_SEED 1
set_env RIABUILD_GITHUB_ORG "$E2E_ORG"
set_env GITHUB_ORG_TOKEN "$E2E_GITHUB_TOKEN"
set_env INFISICAL_SITE_URL "$STUB_URL"
set_env INFISICAL_DEVELOPER_CLIENT_ID "$STUB_CLIENT_ID"
set_env INFISICAL_DEVELOPER_CLIENT_SECRET "$STUB_CLIENT_SECRET"
set_env INFISICAL_PROJECT_ID "e2e-project"
set_env INFISICAL_ENVIRONMENT dev
# The seeded member is a developer, so this run must come back with both files.
# A candidate would get `dev` alone; that split is unit-tested rather than run
# here, because this suite has one seeded member and one checkout.
set_env INFISICAL_STAGING_ENVIRONMENT staging
# Two folders, comma-separated, because one environment's secrets are not
# always in one folder — AI Builders' are in two. The order is the contract:
# riabuild exports them in it and merges later-wins, so the stub puts one key
# in both folders with a value the first one must lose. A single-folder
# deployment is still the ordinary case and is unit-tested; this run covers the
# case that has no other coverage anywhere.
set_env INFISICAL_SECRET_PATH /tenant/e2e/frontend,/tenant/e2e/convex
pass "deployment configured"

