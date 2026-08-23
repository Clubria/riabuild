# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

# ---------------------------------------------------------------------------
# 4. The Infisical stand-in
# ---------------------------------------------------------------------------

step "Infisical stand-in"

STUB_CLIENT_ID="e2e-client-id"
STUB_CLIENT_SECRET="e2e-client-secret-$RANDOM"
export STUB_CLIENT_ID STUB_CLIENT_SECRET

node "$REPO/e2e/infisical-stub.mjs" > "$SCRATCH/stub.log" 2>&1 &
# shellcheck disable=SC2034  # read by teardown(), which stage 02 defined
STUB_PID=$!

STUB_PORT=""
for _ in $(seq 1 50); do
  STUB_PORT="$(sed -n 's/^listening \([0-9]*\)$/\1/p' "$SCRATCH/stub.log" | head -1)"
  [ -n "$STUB_PORT" ] && break
  sleep 0.2
done
[ -n "$STUB_PORT" ] || die "the Infisical stand-in did not start:
$(cat "$SCRATCH/stub.log")"
STUB_URL="http://127.0.0.1:$STUB_PORT"
pass "standing in for app.infisical.com at $STUB_URL"

