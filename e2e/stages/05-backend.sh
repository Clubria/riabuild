# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

# ---------------------------------------------------------------------------
# 5. The backend
# ---------------------------------------------------------------------------

step "Convex backend"

# `set -euo pipefail` is run.sh's and is already in force here; shellcheck
# reads this file on its own and cannot know that.
# shellcheck disable=SC2164
cd "$REPO/riabuild-web"

# `convex dev` writes the deployment it chose into .env.local, so this run needs
# that file to itself. On a developer's laptop it already names *their* dev
# deployment, and losing it is a genuinely annoying thing for a test to do —
# hence the stash, and the restore in teardown.
if [ -f .env.local ]; then
  SAVED_ENV_LOCAL="$SCRATCH/riabuild-web.env.local"
  cp .env.local "$SAVED_ENV_LOCAL"
  info "stashed your riabuild-web/.env.local; teardown puts it back"
fi

# CONVEX_AGENT_MODE=anonymous is what makes this need no Convex account and no
# CONVEX_DEPLOY_KEY: the CLI downloads convex-local-backend and runs it against
# local state. CI therefore cannot reach, or damage, the production deployment.
rm -f .env.local
# From here on the .env.local in the checkout is this run's, which is what lets
# teardown remove it. See stage 02.
# shellcheck disable=SC2034  # read by teardown(), which stage 02 defined
ENV_LOCAL_OURS=1
# `set -m` for exactly this launch, and it is the whole of what makes teardown
# able to stop this backend without stopping anybody else's. Job control puts a
# background job in a process group of its own, so `$!` names a group as well
# as a process and `kill -- -$CONVEX_PID` reaches the `convex-local-backend`
# `npx` spawned — the child the old unscoped `pkill` was there to catch.
# Switched off again immediately: with job control on, later background jobs
# would each get a group too, and the Infisical stand-in above is killed by pid
# and expects to be in this shell's.
set -m
CONVEX_AGENT_MODE=anonymous npx convex dev \
  --tail-logs disable --typecheck disable \
  > "$SCRATCH/convex.log" 2>&1 &
# shellcheck disable=SC2034  # read by teardown(), which stage 02 defined
CONVEX_PID=$!
set +m

for _ in $(seq 1 120); do
  [ -f .env.local ] && grep -q VITE_CONVEX_SITE_URL .env.local && break
  sleep 1
done
grep -q VITE_CONVEX_SITE_URL .env.local 2>/dev/null || die "the local backend never came up:
$(tail -40 "$SCRATCH/convex.log")"

API_URL="$(sed -n 's/^VITE_CONVEX_SITE_URL=//p' .env.local | tr -d '\r')"
# shellcheck disable=SC2034  # read by the riabuild() wrapper stage 10 defines
WEB_URL="$(sed -n 's/^VITE_CONVEX_URL=//p' .env.local | tr -d '\r')"
info "api: $API_URL"

# A 401 from our own endpoint proves two things at once: the backend is
# listening, and convex/http.ts is deployed. Waiting on the port alone races
# with the function push and fails later, further from the cause.
#
# `api_curl`, never a bare `curl`: without `x-riabuild-cli-version` this route
# answers 409 rather than 401, because a missing header is version `0` and no
# `orgConfig` row has been seeded yet — so the floor is the `0.1.0` default.
# See "Talking to /api/v1 directly" in e2e/run.sh. Still asserting 401 rather
# than accepting the 409: 401 is the answer that says the *authentication* path
# this run is about to walk is deployed, and it is the answer that keeps saying
# so once stage 07 has seeded a floor.
READY=""
for _ in $(seq 1 120); do
  if [ "$(api_curl -o /dev/null -w '%{http_code}' "$API_URL/api/v1/me")" = "401" ]; then
    READY=1
    break
  fi
  sleep 1
done
[ -n "$READY" ] || die "/api/v1/me never answered on $API_URL:
$(tail -40 "$SCRATCH/convex.log")"
pass "backend up and /api/v1 deployed"

