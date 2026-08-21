#!/usr/bin/env bash
#
# The riabuild end-to-end test: the real CLI, provisioning a real machine,
# against a real backend.
#
# What this covers that `cargo test` structurally cannot:
#
#   - that the Rust client deserialises what Convex actually serves. A field
#     renamed in convex/http.ts passes every unit test on both sides and breaks
#     every laptop.
#   - that `apply()` is safe to run twice, on a machine rather than in a tempdir.
#   - that `check()` sees real drift and repairs only what drifted.
#   - that `security(1)`, the Node tarball download, the generated rcfiles and
#     the shell handoff work on macOS, which is the platform riabuild ships to.
#   - that CLAUDE_CONFIG_DIR still redirects Claude Code. It is undocumented, so
#     it is only a promise for as long as a test says so.
#
# What is faked, and why exactly one thing is:
#
#   app.infisical.com -> e2e/infisical-stub.mjs. Everything else — Convex,
#   GitHub, the Node tarball, Homebrew, npm — is the real service. Putting a
#   real Infisical machine identity in CI would place the credential that
#   unlocks every dev secret into GitHub Actions in order to test code we own.
#
# Usage:
#   E2E_GITHUB_TOKEN=<token> e2e/run.sh
#
# Environment:
#   E2E_GITHUB_TOKEN  required. A token belonging to a *user* who is an active
#                     member of the GitHub org. Actions' built-in GITHUB_TOKEN
#                     cannot be used: it is not a user, so the membership call
#                     it has to answer returns 403 no matter how it is scoped.
#   E2E_KEEP=1        leave the scratch directory, backend and stub running.
#   RIABUILD_BIN      skip `cargo build` and test this binary instead.
#
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Cloned instead of the private repo: it exercises the identical clone,
# origin-verification and repo_status paths, in seconds, without putting a
# checkout of the product codebase on a hosted runner. `check()` compares the
# remote against whatever the server says, so the slug being a stand-in is
# invisible to every line of code under test.
E2E_REPO_SLUG="${E2E_REPO_SLUG:-Clubria/riabuild}"
E2E_REPO_NAME="${E2E_REPO_SLUG##*/}"

# Below the `9999.0.0-dev` a local build reports, so the run under test never
# decides it is out of date and replaces its own binary with `brew upgrade`.
E2E_MIN_CLI_VERSION="2026.01.01"
E2E_LATEST_CLI_VERSION="2026.01.01"

# Distinctive enough that finding it in org-settings.json proves the file came
# from this deployment rather than from a developer's real cache.
E2E_CLAUDE_SETTINGS='{"env":{"CLUBRIA_E2E":"1"},"permissions":{"deny":["Read(./.env.dev)","Read(./.env.staging)"]}}'

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------

STEP=0
step() { STEP=$((STEP + 1)); printf '\n\033[1m=== %d. %s\033[0m\n' "$STEP" "$*"; }
info() { printf '    %s\n' "$*"; }
pass() { printf '    \033[32mok\033[0m   %s\n' "$*"; }

FAILURES=0
fail() {
  printf '    \033[31mFAIL\033[0m %s\n' "$*" >&2
  FAILURES=$((FAILURES + 1))
}

# Fatal: the run cannot continue and every later assertion would be noise.
die() {
  printf '\n\033[31m%s\033[0m\n' "$*" >&2
  exit 1
}

check() {
  local what="$1"
  shift
  if "$@" >/dev/null 2>&1; then pass "$what"; else fail "$what"; fi
}

check_contains() {
  local what="$1" haystack="$2" needle="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    pass "$what"
  else
    fail "$what — expected to find: $needle"
    printf '%s\n' "$haystack" | sed 's/^/         | /' >&2
  fi
}

check_missing() {
  local what="$1" haystack="$2" needle="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    fail "$what — did not expect: $needle"
  else
    pass "$what"
  fi
}

# ---------------------------------------------------------------------------
# 1. Preflight
# ---------------------------------------------------------------------------

step "Preflight"

case "$(uname -s)" in
  Darwin) PLATFORM=macos ;;
  # Linux runs everything except the Keychain assertions, so the flow can be
  # debugged without a Mac. The macOS runner is what makes the run authoritative.
  Linux) PLATFORM=linux ;;
  *) die "riabuild targets macOS; this is $(uname -s)." ;;
esac
info "platform: $PLATFORM"

for tool in cargo node npx pnpm gh git curl python3; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is not installed."
done

# `gh` and `infisical` are riabuild's to install on both platforms now, so
# nothing has to be staged for it — the tasks that fetch them are part of what
# this run is testing.

if [ -z "${E2E_GITHUB_TOKEN:-}" ]; then
  die "E2E_GITHUB_TOKEN is not set.

The end-to-end run needs a GitHub token belonging to a *user* who is an active
member of the org, because riabuild checks membership from both sides:

  - the CLI's github_cli task runs \`gh api /user/memberships/orgs/<org>\`
  - riabuild-web re-verifies membership before brokering any secret

Actions' built-in GITHUB_TOKEN is an installation token, not a user, and gets a
403 from both regardless of permissions. Create a fine-grained PAT with
Organization permissions -> Members: Read and store it as the E2E_GITHUB_TOKEN
repository secret."
fi

# gh reads GH_TOKEN from the environment, which keeps this out of the runner's
# gh config entirely — nothing to write, nothing to clean up, and no chance of
# picking up an ambient login that would make a green run meaningless.
export GH_TOKEN="$E2E_GITHUB_TOKEN"

# ---------------------------------------------------------------------------
# 2. Scratch space and teardown
# ---------------------------------------------------------------------------

step "Scratch space"

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/riabuild-e2e.XXXXXX")"
E2E_HOME="$SCRATCH/home"
mkdir -p "$E2E_HOME"
info "scratch: $SCRATCH"

CONVEX_PID=""
STUB_PID=""
KEYCHAIN=""
SAVED_ENV_LOCAL=""

# Everything worth reading after a failure, with the two live credentials
# scrubbed. Copied out rather than kept in place because the scratch tree also
# holds the seeded session token, and a CI artifact is a published thing.
save_logs() {
  local out="$REPO/e2e-logs"
  mkdir -p "$out"
  for log in convex stub; do
    [ -f "$SCRATCH/$log.log" ] || continue
    sed -e "s|$E2E_GITHUB_TOKEN|<E2E_GITHUB_TOKEN>|g" \
        -e "s|${SESSION_TOKEN:-__none__}|<SESSION_TOKEN>|g" \
        "$SCRATCH/$log.log" > "$out/$log.log"
  done
  printf 'logs saved to %s\n' "$out"
}

teardown() {
  local status=$?
  [ "$status" -ne 0 ] && save_logs || true
  if [ "${E2E_KEEP:-}" = "1" ]; then
    printf '\nE2E_KEEP=1: leaving %s, backend and stub running.\n' "$SCRATCH"
    return $status
  fi
  printf '\n--- teardown ---\n'
  [ -n "$CONVEX_PID" ] && kill "$CONVEX_PID" 2>/dev/null || true
  [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null || true
  # `convex dev` spawns the backend as a child that does not always go with it.
  pkill -f convex-local-backend 2>/dev/null || true
  # The keychain and the search list that names it both live inside the scratch
  # tree, so the developer's own keychains were never touched and there is
  # nothing to put back. Deleting it explicitly just tidies up securityd's view.
  [ -n "$KEYCHAIN" ] && env HOME="$E2E_HOME" security delete-keychain "$KEYCHAIN" 2>/dev/null || true
  if [ -n "$SAVED_ENV_LOCAL" ] && [ -f "$SAVED_ENV_LOCAL" ]; then
    cp "$SAVED_ENV_LOCAL" "$REPO/riabuild-web/.env.local"
    printf 'restored riabuild-web/.env.local\n'
  else
    # Ours, and only ours: it names an anonymous deployment nothing else uses.
    rm -f "$REPO/riabuild-web/.env.local"
  fi
  rm -rf "$SCRATCH"
  return $status
}
trap teardown EXIT

# ---------------------------------------------------------------------------
# 3. Who the token is
# ---------------------------------------------------------------------------

step "GitHub identity"

E2E_ORG="${E2E_ORG:-Clubria}"

# `gh api` writes an HTTP error *body to stdout* and exits non-zero, and it does
# not apply `--jq` to that body. So `$(gh api … 2>/dev/null || true)` captures
# the error JSON as though it were the field asked for — and the result is
# non-empty, which is precisely what a `[ -n … ]` guard tests. A discarded exit
# status is therefore not a small omission here; it inverts the check.
#
# On 2026-08-17 GitHub returned 503 during a partial outage and `E2E_LOGIN`
# became `{"message": "No server is currently available…"}`. It passed this
# guard, was printed as the developer's name, and brought the run down four
# steps later in the seed with `SyntaxError: JSON5: invalid character 'm'` —
# naming neither GitHub nor this line. The old message could not have been
# right either way: it offered "expired, revoked, or not a user token" for a
# failure that was none of those.
#
# So: take the exit status, keep gh's own reason for the message, and validate
# the shape before anything downstream is handed the value.
if ! E2E_LOGIN="$(gh api /user --jq .login 2>"$SCRATCH/github-identity.err")"; then
  die "E2E_GITHUB_TOKEN could not read /user, so this run cannot start.

It is expired or revoked, it is not a user token, or GitHub is unavailable —
check https://www.githubstatus.com before assuming it is the token.

gh said:
$(cat "$SCRATCH/github-identity.err")
$E2E_LOGIN"
fi

# A GitHub login is letters, digits and hyphens, so anything else is not one,
# whatever gh exited with. This is the strict half: every step after this
# interpolates the value into a Convex argument, a path, or a message, and none
# of them can tell a login from an error that happens to be a string.
if [ -z "$E2E_LOGIN" ]; then
  die "GitHub answered /user with no login at all, so this run cannot start.
The token may not be a user token."
elif [[ ! $E2E_LOGIN =~ ^[A-Za-z0-9-]+$ ]]; then
  die "GitHub answered /user with something that is not a login:

$E2E_LOGIN"
fi
info "token belongs to @$E2E_LOGIN"

# Asserted here rather than discovered six steps later as a confusing task
# failure. Everything after this point assumes the answer is yes.
#
# A failure here is *not* normalised away: 404 is how GitHub says "not a
# member" and 403 is how it says "this token may not ask", so an unreadable
# state is a real answer to report rather than an error to retry. What it must
# not do is report only the permissions remedy, because a 5xx lands here too —
# hence gh's own words below the guidance.
MEMBERSHIP_REASON=""
if ! MEMBERSHIP="$(gh api "/user/memberships/orgs/$E2E_ORG" --jq .state 2>"$SCRATCH/github-membership.err")"; then
  MEMBERSHIP_REASON="$(cat "$SCRATCH/github-membership.err")
$MEMBERSHIP"
  MEMBERSHIP="unreadable"
fi
[ "$MEMBERSHIP" = "active" ] || die "@$E2E_LOGIN is not an active member of $E2E_ORG (state: ${MEMBERSHIP:-unreadable}).

If the state is unreadable, the token is missing organisation read access:
a fine-grained PAT needs Organization permissions -> Members: Read, with
$E2E_ORG as the resource owner, and the org has to approve the token.
It can also mean GitHub is unavailable — check https://www.githubstatus.com.
${MEMBERSHIP_REASON:+
gh said:
$MEMBERSHIP_REASON}"
pass "@$E2E_LOGIN is an active member of $E2E_ORG"

# ---------------------------------------------------------------------------
# 4. The Infisical stand-in
# ---------------------------------------------------------------------------

step "Infisical stand-in"

STUB_CLIENT_ID="e2e-client-id"
STUB_CLIENT_SECRET="e2e-client-secret-$RANDOM"
export STUB_CLIENT_ID STUB_CLIENT_SECRET

node "$REPO/e2e/infisical-stub.mjs" > "$SCRATCH/stub.log" 2>&1 &
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

# ---------------------------------------------------------------------------
# 5. The backend
# ---------------------------------------------------------------------------

step "Convex backend"

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
CONVEX_AGENT_MODE=anonymous npx convex dev \
  --tail-logs disable --typecheck disable \
  > "$SCRATCH/convex.log" 2>&1 &
CONVEX_PID=$!

for _ in $(seq 1 120); do
  [ -f .env.local ] && grep -q VITE_CONVEX_SITE_URL .env.local && break
  sleep 1
done
grep -q VITE_CONVEX_SITE_URL .env.local 2>/dev/null || die "the local backend never came up:
$(tail -40 "$SCRATCH/convex.log")"

API_URL="$(sed -n 's/^VITE_CONVEX_SITE_URL=//p' .env.local | tr -d '\r')"
WEB_URL="$(sed -n 's/^VITE_CONVEX_URL=//p' .env.local | tr -d '\r')"
info "api: $API_URL"

# A 401 from our own endpoint proves two things at once: the backend is
# listening, and convex/http.ts is deployed. Waiting on the port alone races
# with the function push and fails later, further from the cause.
READY=""
for _ in $(seq 1 120); do
  if [ "$(curl -s -o /dev/null -w '%{http_code}' "$API_URL/api/v1/me")" = "401" ]; then
    READY=1
    break
  fi
  sleep 1
done
[ -n "$READY" ] || die "/api/v1/me never answered on $API_URL:
$(tail -40 "$SCRATCH/convex.log")"
pass "backend up and /api/v1 deployed"

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
set_env INFISICAL_SECRET_PATH /
pass "deployment configured"

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

# ---------------------------------------------------------------------------
# 8. Build
# ---------------------------------------------------------------------------

step "Build"

if [ -n "${RIABUILD_BIN:-}" ]; then
  RIABUILD="$RIABUILD_BIN"
else
  # No RIABUILD_VERSION: the binary reports the 9999.0.0-dev sentinel, which
  # clears minCliVersion and sits above latestCliVersion, so the run under test
  # never upgrades itself out from under the assertions.
  (cd "$REPO/riabuild-cli" && cargo build --locked)
  RIABUILD="$REPO/riabuild-cli/target/debug/riabuild"
fi
[ -x "$RIABUILD" ] || die "no riabuild binary at $RIABUILD"
info "binary: $RIABUILD ($("$RIABUILD" --version))"

# ---------------------------------------------------------------------------
# 9. The session token, where riabuild expects to find it
# ---------------------------------------------------------------------------

step "Session token"

if [ "$PLATFORM" = macos ]; then
  # Every `security` call below runs with the same redirected HOME riabuild will
  # run with, and that is the whole trick.
  #
  # The keychain *search list* is a per-user preference living in
  # ~/Library/Preferences, not a global session setting. Register a keychain
  # against the real home and a process whose HOME points elsewhere cannot see
  # it — `security find-generic-password` searches a list that is simply not
  # there. An earlier version of this set the keychain up with the real HOME,
  # verified the read, passed, and then left riabuild reporting "not signed in"
  # and starting a sign-in nobody was there to approve. The verification passed
  # *because* it ran in the wrong environment.
  KEYCHAIN="$SCRATCH/riabuild-e2e.keychain-db"
  mkdir -p "$E2E_HOME/Library/Preferences"
  keychain() { env HOME="$E2E_HOME" security "$@"; }

  keychain create-keychain -p e2e "$KEYCHAIN"
  keychain set-keychain-settings "$KEYCHAIN"   # no auto-lock
  keychain unlock-keychain -p e2e "$KEYCHAIN"
  # Ours is the only entry: nothing else in this throwaway home needs a
  # keychain, so there is no real search list to save and put back either.
  keychain list-keychains -s "$KEYCHAIN"
  # The keychain is the trailing positional argument — `add-generic-password`
  # has no -k, and passing one makes it fail rather than fall back.
  #
  # -A grants every application access. Without it the first read raises a GUI
  # authorisation prompt, and on a headless runner that is an indefinite hang
  # rather than a failure.
  keychain add-generic-password -U -A \
    -s com.clubria.riabuild -a session-token -w "$SESSION_TOKEN" "$KEYCHAIN"

  # Read it back the way riabuild does, in the environment riabuild does it in.
  STORED="$(keychain find-generic-password -s com.clubria.riabuild -a session-token -w 2>&1 || true)"
  [ "$STORED" = "$SESSION_TOKEN" ] \
    || die "the token did not come back out of the Keychain (got: ${STORED:0:40})"
  pass "token stored in a dedicated Keychain and readable the way riabuild reads it"
else
  # riabuild's own escape hatch for machines with no keyring. It skips
  # keychain.rs, which is why the macOS runner is the authoritative one.
  export RIABUILD_TOKEN="$SESSION_TOKEN"
  info "no Keychain on Linux — using the RIABUILD_TOKEN escape hatch"
fi

# ---------------------------------------------------------------------------
# 10. Run riabuild
# ---------------------------------------------------------------------------

RIA_HOME="$E2E_HOME/.riabuild"
LOG="$RIA_HOME/logs/riabuild.log"

# The one place this run has to put its thumb on the scale, and it is worth
# being precise about why.
#
# The task engine treats "no record in state.json" as `NeverRun` and applies the
# task *without calling check() first* — by design, and documented as rule 1 in
# the spec. For `login` that means a fresh sign-in on any machine with no state
# file, however good the session already in the Keychain is. There is no human
# here to approve a device code, so it would print one and poll until the code
# expired fifteen minutes later.
#
# So `login` starts with a record, and only `login`. Every other task meets a
# genuinely empty state file and runs for real. What is skipped is the human
# approval, which is un-automatable by construction; what is still exercised is
# the session it produces — every request after this authenticates with the real
# token against the real endpoint.
mkdir -p "$RIA_HOME"
printf '{"tasks":{"login":{"version":1,"last_ok_at":%s,"last_reason":"e2e_seeded_session"}}}\n' \
  "$(date +%s)" > "$RIA_HOME/state.json"

# The task ids present before anything runs, so the dry run can be held to
# adding none of its own.
state_tasks() {
  python3 -c "import json;print(' '.join(sorted(json.load(open('$RIA_HOME/state.json'))['tasks'])))" 2>/dev/null || echo ""
}

# HOME is redirected so the run provisions a machine of its own: ~/.riabuild,
# the checkout, and the Claude profiles all land in the scratch tree and go away
# with it. The Keychain is unaffected — its search list is a session setting,
# not a HOME-relative one, which is why step 9 works at all.
riabuild() {
  env HOME="$E2E_HOME" \
      GH_TOKEN="$E2E_GITHUB_TOKEN" \
      RIABUILD_API_URL="$API_URL" \
      RIABUILD_WEB_URL="$WEB_URL" \
      ${RIABUILD_TOKEN:+RIABUILD_TOKEN="$RIABUILD_TOKEN"} \
      "$RIABUILD" "$@"
}

# The last line of the run log, which records `applied=[...]`. Machine-readable
# and stable, unlike the human-facing output, which is meant to change.
last_run_log() { tail -1 "$LOG" 2>/dev/null || echo "(no log)"; }

step "riabuild --check, on a machine with nothing set up"

TASKS_BEFORE="$(state_tasks)"
CHECK_OUT="$(riabuild --check --no-shell 2>&1)" && CHECK_RC=0 || CHECK_RC=$?
[ "$CHECK_RC" = 0 ] && pass "exited 0" || { fail "exited $CHECK_RC"; printf '%s\n' "$CHECK_OUT" | sed 's/^/         | /'; }

# `--check` promises to change nothing. It does still rewrite state.json and the
# ~/.riabuild/bin shims — see e2e/README.md — so what is asserted here is the
# part that would actually mislead the next run: a dry run must never record a
# task as satisfied, or the real run would skip it.
[ "$(state_tasks)" = "$TASKS_BEFORE" ] \
  && pass "the dry run recorded no task as done" \
  || fail "the dry run recorded tasks: was [$TASKS_BEFORE], now [$(state_tasks)]"
check_contains "it reported work to do" "$CHECK_OUT" "would run"
# Where the CLI puts a checkout is its decision, and differs per platform, so
# this asks the question the same way the CLI answers it.
if [ "$PLATFORM" = macos ]; then
  DEFAULT_CHECKOUT="$E2E_HOME/Documents/Clubria/$E2E_REPO_NAME"
else
  # Linux groups checkouts under ~/Clubria — the same grouping macOS puts
  # inside ~/Documents. This must track `paths::default_project_dir`: the
  # assertion below is that the directory does *not* exist, so a stale path
  # here does not fail, it passes without checking anything.
  DEFAULT_CHECKOUT="$E2E_HOME/Clubria/$E2E_REPO_NAME"
fi
check "the dry run cloned nothing" test ! -d "$DEFAULT_CHECKOUT"

step "riabuild, for real"

# Exactly one provisioning failure is tolerated, and only when it is this one:
# `claude auth login` opens a browser somebody has to finish, and a CI runner has
# nobody to finish it. riabuild refusing there is riabuild working — the spec
# makes a signed-in account 1 a blocking requirement — so what this asserts is
# that the refusal is *that* refusal, arriving in a sentence rather than as a
# thirty-minute hang, which is what it used to be.
#
# A branch, not a lowered bar, and it re-arms itself: seed a signed-in Claude Code
# config directory under ~/.riabuild/claude before the run — `claude_accounts`
# adopts a directory it finds on disk — and provisioning succeeds, SIGN_IN stays
# `done`, and every gated assertion below runs in place of its substitute with
# nothing to remember to put back. Each gate says what it is standing in for; the
# whole arrangement is written up in e2e/README.md.
SIGN_IN=done
if ! PROVISION_OUT="$(riabuild --no-shell 2>&1)"; then
  case "$PROVISION_OUT" in
    *'no terminal to hand the sign-in to'*) SIGN_IN=refused ;;
    *)
      printf '%s\n' "$PROVISION_OUT" | sed 's/^/         | /' >&2
      die "riabuild failed to provision the machine."
      ;;
  esac
fi
printf '%s\n' "$PROVISION_OUT" | sed 's/^/         | /'

if [ "$SIGN_IN" = done ]; then
  pass "provisioned"
else
  pass "provisioned as far as a machine with nobody at the keyboard goes"
  # The refusal has to name the step, and name one thing a person can do. A
  # provisioner that stops without either is indistinguishable from a crash.
  check_contains "it stopped at the Claude Code sign-in and named it" \
    "$PROVISION_OUT" "signing you in to Claude Code"
  check_contains "and named the one action that finishes it" \
    "$PROVISION_OUT" 'Run `riabuild` yourself from a terminal'
  info "no Anthropic credential here, so the sign-in and its dependants are asserted short of done"
fi

# ---------------------------------------------------------------------------
# 11. What it did to the machine
# ---------------------------------------------------------------------------

step "The machine riabuild built"

STATE="$(cat "$RIA_HOME/state.json" 2>/dev/null || echo '{}')"
for task in login github_cli git_credentials infisical_cli ngrok toolchain project \
            repo_status codex_cli grok_cli org_settings env_local claude_statusline; do
  check_contains "task recorded: $task" "$STATE" "\"$task\""
done

# `git_credentials` is asserted against the machine as well as against
# `state.json`, and this suite is the exact case it exists for: gh is signed in
# from `GH_TOKEN`, so `github_cli` is satisfied on its first check and the
# `setup-git` inside its own sign-in path never runs. A recorded task proves
# only that riabuild believed itself done, so the config is read back — with
# `HOME` set the way riabuild had it, since the helper goes in that home's
# global gitconfig and nowhere else.
check_contains "git asks riabuild's own gh for github.com credentials" \
  "$(env HOME="$E2E_HOME" git config --get-all \
      'credential.https://github.com.helper' 2>/dev/null || echo '')" \
  "$RIA_HOME/gh/"

# `codex_cli` is asserted on *both* paths, unlike the four below, and that is
# the point of it being here: it waits on the toolchain and on nothing else, so
# a developer who walked away from the Claude sign-in still has a working Codex.
# It is declared ahead of `claude_accounts` in `registry()` precisely so that
# stays true — an aborted apply ends the run, so a task behind the one browser
# round trip would never run on the machine that most needs it to.
check "the codex launcher is there" test -x "$RIA_HOME/bin/codex"
check_contains "the codex launcher adds --yolo" \
  "$(cat "$RIA_HOME/bin/codex" 2>/dev/null || echo '')" "--yolo"

# ngrok is installed but never authenticated on disk: the launcher fetches the
# team's authtoken on every invocation and puts it in that one process's
# environment. A token written into ngrok.yml, an rcfile, or this script would
# be the thing the whole design exists to avoid, so the assertion is about what
# is *absent* as much as what is there.
check "the ngrok launcher is there" test -x "$RIA_HOME/bin/ngrok"
check_contains "the ngrok launcher fetches the token per invocation" \
  "$(cat "$RIA_HOME/bin/ngrok" 2>/dev/null || echo '')" "internal ngrok-token"
check "riabuild wrote no ngrok config" \
  test ! -f "$HOME/Library/Application Support/ngrok/ngrok.yml"

# All nine, each with its own directory and its own CODEX_HOME. Codex keeps
# sign-ins apart per CODEX_HOME and by nothing else, so nine launchers sharing
# one would be nine names for a single account — and every other assertion here
# would still pass. `CODEX_HOMES` collects them so the distinctness of the set
# can be asserted rather than just the presence of each.
CODEX_HOMES=""
for n in 1 2 3 4 5 6 7 8 9; do
  check "codex profile $n has a config directory" test -d "$RIA_HOME/codex/$n"
  check "the codex-$n launcher is there" test -x "$RIA_HOME/bin/codex-$n"
  check_contains "codex-$n pins its own CODEX_HOME" \
    "$(cat "$RIA_HOME/bin/codex-$n" 2>/dev/null || echo '')" \
    "CODEX_HOME=\"$RIA_HOME/codex/$n\""
  CODEX_HOMES="$CODEX_HOMES$(sed -n 's/^CODEX_HOME="\(.*\)"$/\1/p' \
    "$RIA_HOME/bin/codex-$n" 2>/dev/null | head -1)
"
done
CODEX_DISTINCT="$(printf '%s' "$CODEX_HOMES" | sort -u | grep -c . || true)"
if [ "$CODEX_DISTINCT" = "9" ]; then
  pass "the nine codex launchers open nine different accounts"
else
  fail "the codex launchers share a CODEX_HOME — $CODEX_DISTINCT distinct, expected 9"
fi

# `codex` and `codex-1` are one account under two names, the shape `claude` and
# `claude-1` already have.
if cmp -s "$RIA_HOME/bin/codex" "$RIA_HOME/bin/codex-1"; then
  pass "the bare codex launcher is the first profile"
else
  fail "the bare codex launcher is not codex-1"
fi

# Grok Build sits beside Codex for the same reason and is asserted on both paths:
# it depends on nothing the Claude sign-in provides, so a developer who walked
# away from the browser still has a working `grok`. Unlike Codex it waits on the
# toolchain too — it is a static binary, so it has no `depends_on` at all.
check "the grok launcher is there" test -x "$RIA_HOME/bin/grok"

# The whole point of the wrapper. `bypassPermissions` and not `dontAsk`, which
# reads like the same thing and silently *denies* every tool that is not
# pre-approved — a session that looks permissive and does nothing.
check_contains "the grok launcher bypasses permissions" \
  "$(cat "$RIA_HOME/bin/grok" 2>/dev/null || echo '')" \
  "--permission-mode bypassPermissions"

# riabuild downloads the binary itself and never runs xAI's installer, which is a
# competing provisioner: it writes ~/.grok/bin, symlinks into /usr/local/bin, and
# appends a PATH block to the developer's rcfile — the one thing that would
# demote ~/.riabuild/bin and quietly break the claude launcher and the clipboard
# shims beside it. Asserted as an absence, like the ngrok config above.
check "riabuild wrote no grok bin directory of xAI's" test ! -d "$HOME/.grok/bin"
check "riabuild left the developer's own ~/.grok alone" test ! -f "$HOME/.grok/config.toml"

# All nine, each with its own directory and its own GROK_HOME. Grok Build keeps
# sign-ins apart per GROK_HOME and by nothing else, so nine launchers sharing one
# would be nine names for a single account — and every other assertion here would
# still pass.
GROK_HOMES=""
for n in 1 2 3 4 5 6 7 8 9; do
  check "grok profile $n has a config directory" test -d "$RIA_HOME/grok/$n"
  check "the grok-$n launcher is there" test -x "$RIA_HOME/bin/grok-$n"
  check_contains "grok-$n pins its own GROK_HOME" \
    "$(cat "$RIA_HOME/bin/grok-$n" 2>/dev/null || echo '')" \
    "GROK_HOME=\"$RIA_HOME/grok/$n\""
  check_contains "grok-$n bypasses permissions" \
    "$(cat "$RIA_HOME/bin/grok-$n" 2>/dev/null || echo '')" \
    "--permission-mode bypassPermissions"
  GROK_HOMES="$GROK_HOMES$(sed -n 's/^GROK_HOME="\(.*\)"$/\1/p' \
    "$RIA_HOME/bin/grok-$n" 2>/dev/null | head -1)
"
done
GROK_DISTINCT="$(printf '%s' "$GROK_HOMES" | sort -u | grep -c . || true)"
if [ "$GROK_DISTINCT" = "9" ]; then
  pass "the nine grok launchers open nine different accounts"
else
  fail "the grok launchers share a GROK_HOME — $GROK_DISTINCT distinct, expected 9"
fi

# `grok` and `grok-1` are one account under two names, the shape `claude` and
# `codex` already have.
if cmp -s "$RIA_HOME/bin/grok" "$RIA_HOME/bin/grok-1"; then
  pass "the bare grok launcher is the first profile"
else
  fail "the bare grok launcher is not grok-1"
fi

# The four tasks the sign-in gates. `claude_accounts` is only recorded once
# account 1 is actually signed in; `claude_trust`, `claude_onboarding` and
# `claude_agents_view` all write per-account state into a `.claude.json` that has
# no account to belong to yet, so none of them runs at all.
#
# Short of the sign-in this asserts their *absence*, which is the more valuable
# half of the pair: "never record a success we have not verified" is the invariant
# the whole task engine rests on, and a run that got nine tasks done and stopped
# at the tenth is precisely the situation in which a provisioner is tempted to
# round up. A recorded claude_accounts here would mean the next run skipped the
# sign-in and left the developer with an account they cannot use — and a recorded
# claude_onboarding would mean it skipped the one write that keeps Claude Code
# from interviewing them on first launch.
for task in claude_accounts claude_trust claude_onboarding claude_agents_view; do
  if [ "$SIGN_IN" = done ]; then
    check_contains "task recorded: $task" "$STATE" "\"$task\""
  else
    check_missing "not recorded, because the sign-in did not finish: $task" "$STATE" "\"$task\""
  fi
done

CONFIG="$(cat "$RIA_HOME/config.json" 2>/dev/null || echo '{}')"
read_config() { printf '%s' "$CONFIG" | python3 -c "import json,sys; print(json.load(sys.stdin).get('$1') or '')"; }
read_config_list_first() {
  printf '%s' "$CONFIG" | python3 -c "import json,sys; v=json.load(sys.stdin).get('$1') or []; print(v[0] if v else '')"
}
# The checkout of the repository this machine is working on.
#
# `config.json` holds a *map* of checkouts since riabuild began asking which
# repository to work on, keyed by `owner/repo`, with `active_repo` naming the one
# in use. `project_path` is what riabuild wrote before that and is read here as a
# fallback for exactly one reason: this suite must keep passing against a
# `config.json` an older riabuild left behind.
read_active_checkout() {
  printf '%s' "$CONFIG" | python3 -c "
import json, sys

config = json.load(sys.stdin)
repos = config.get('repos') or {}
active = config.get('active_repo')
print(repos.get(active) or config.get('project_path') or '')
"
}
read_active_repo() {
  printf '%s' "$CONFIG" | python3 -c "import json,sys; print(json.load(sys.stdin).get('active_repo') or '')"
}

NODE_VERSION="$(read_config node_version)"
PNPM_VERSION="$(read_config pnpm_version)"
PROJECT_DIR="$(read_active_checkout)"
ACTIVE_REPO="$(read_active_repo)"
CLAUDE_ACCOUNT="$(read_config_list_first claude_accounts)"
info "node=$NODE_VERSION pnpm=$PNPM_VERSION account=$CLAUDE_ACCOUNT"
info "checkout=$PROJECT_DIR repo=$ACTIVE_REPO"

# The repository picker's own record. A first run has no session when the
# question would be put, so it provisions the org default and records that —
# which is the repository this suite's checkout has to be of.
check_contains "the repository riabuild recorded is the one the server named" \
  "$ACTIVE_REPO" "$E2E_REPO_SLUG"

check_contains "riabuild's Node is the version it pinned" \
  "$("$RIA_HOME/node/$NODE_VERSION/bin/node" -v 2>&1)" "v$NODE_VERSION"
check_contains "riabuild's pnpm is the version it pinned" \
  "$("$RIA_HOME/bin/pnpm" --version 2>&1)" "$PNPM_VERSION"
# True on every path, and worth asking on every path: nothing in riabuild may
# create `c` any more, including the code that writes the launchers it replaced.
check "the retired c launcher is gone" test ! -e "$RIA_HOME/bin/c"
# Written after the task engine finishes, so a run that stops at the sign-in
# writes none of them. That is the engine's ordinary fail-fast contract rather
# than anything specific to accounts — a failed `project` task costs the shell
# too — so there is nothing here to assert short of a completed run.
if [ "$SIGN_IN" = done ]; then
  check "the claude launcher is executable" test -x "$RIA_HOME/bin/claude"
  check "the first account's launcher is executable" test -x "$RIA_HOME/bin/claude-1"
else
  info "launchers not checked: the run stopped before the step that writes them"
fi

check "the checkout is a git repository" test -d "$PROJECT_DIR/.git"
check_contains "the checkout's origin is the repo the server named" \
  "$(git -C "$PROJECT_DIR" remote get-url origin 2>&1)" "$E2E_REPO_NAME"

check "org-settings.json is valid JSON" \
  python3 -c "import json;json.load(open('$RIA_HOME/org-settings.json'))"
check_contains "org-settings.json is what this deployment served" \
  "$(cat "$RIA_HOME/org-settings.json" 2>/dev/null)" "CLUBRIA_E2E"

check "the first account's config directory exists" test -d "$RIA_HOME/claude/$CLAUDE_ACCOUNT"

# The org settings *name* this script; the binary carries it. That split is what
# keeps a dashboard field from being a way to run code on a laptop, so the file
# has to actually arrive from the binary for the settings to mean anything.
check "the status line script was installed from the binary" \
  test -s "$RIA_HOME/claude-statusline.js"

# The whole reason riabuild exists: a developer ends up with working secrets.
ENV_DEV="$PROJECT_DIR/.env.dev"
check "the project has a .env.dev" test -f "$ENV_DEV"
check_contains "the secrets came through the broker" \
  "$(cat "$ENV_DEV" 2>/dev/null)" "CLUBRIA_E2E_MARKER"
check ".env.dev is ignored by git" \
  git -C "$PROJECT_DIR" check-ignore -q .env.dev

# A developer may see staging, so the same run must have pulled it as well —
# into its own file, from its own environment.
ENV_STAGING="$PROJECT_DIR/.env.staging"
check "the project has a .env.staging" test -f "$ENV_STAGING"
check ".env.staging is ignored by git" \
  git -C "$PROJECT_DIR" check-ignore -q .env.staging
# The two files must not be the same export under two names. The stub serves a
# different marker per environment precisely so this can be asserted: without
# it, pulling `dev` twice would satisfy every check above.
check_contains "staging secrets came from the staging environment" \
  "$(cat "$ENV_STAGING" 2>/dev/null)" "brokered-through-riabuild-staging"
check_missing "the dev file did not get staging's secrets" \
  "$(cat "$ENV_DEV" 2>/dev/null)" "brokered-through-riabuild-staging"

check_missing "no secret was written into ~/.riabuild" \
  "$(grep -rl "brokered-through-riabuild" "$RIA_HOME" 2>/dev/null || true)" "$RIA_HOME"

# The stub proves the request actually reached "Infisical", rather than the
# assertions above passing on a file left behind by something else.
check_contains "riabuild-web brokered a token" \
  "$(cat "$SCRATCH/stub.log")" "POST /api/v1/auth/universal-auth/login"
check_contains "the CLI fetched secrets with it" \
  "$(cat "$SCRATCH/stub.log")" "GET /api/v4/secrets"
check_missing "the stand-in was never asked for anything it does not implement" \
  "$(cat "$SCRATCH/stub.log")" "unimplemented"

# ---------------------------------------------------------------------------
# 12. The invariant the whole task engine rests on
# ---------------------------------------------------------------------------

step "A second run changes nothing"

# The `applied=[...]` field of the run log, which names task ids rather than the
# human-facing titles. `--check` writes this log too, and that is what makes it a
# usable stand-in below: the same field, from a command that completes on a
# machine where a real run cannot.
applied_ids() {
  printf '%s' "$1" | sed -n 's/.*applied=\[\(.*\)\]$/\1/p'
}

if [ "$SIGN_IN" = done ]; then
  riabuild --no-shell >/dev/null 2>&1 || fail "the second run did not exit 0"
  SECOND="$(last_run_log)"
  info "$SECOND"
  check_contains "nothing was applied the second time" "$SECOND" "applied=[]"
else
  # Same invariant, asked in the one way an unattended machine can answer it.
  # A real second run stops at the sign-in again and never reaches the code that
  # writes the run log, so there is no `applied=[]` to read. `--check` runs every
  # task's status and applies nothing, so it completes — and what it must report
  # is the four tasks the sign-in blocks and *nothing else*. That is the same
  # claim `applied=[]` makes, minus the one item this environment cannot supply.
  #
  # Four, not one: `claude_trust`, `claude_onboarding` and `claude_agents_view`
  # each write per-account state into a `.claude.json` that only exists once an
  # account does, so a missing sign-in blocks all three. `claude_plugins` sits in
  # the same wave and is *not* here, because it answers Satisfied for a checkout
  # that declares no plugins — which is the shape of this one.
  #
  # The order is the engine's, not alphabetical: within a dependency wave, tasks
  # run in registry declaration order. A reordering of `registry()` that is
  # otherwise harmless will fail here, and that is the assertion doing its job —
  # the order a developer watches scroll past is part of the interface.
  #
  # Their reason is "first run", not "account 1 is not signed in": `status_for`
  # answers a task with no state record without calling `check()` at all. Which
  # is why this asserts the set of task ids and not the sentence.
  if CHECK_AFTER="$(riabuild --check --no-shell 2>&1)"; then
    pass "a --check after the aborted run still exits 0"
  else
    fail "a --check after the aborted run did not exit 0"
    printf '%s\n' "$CHECK_AFTER" | sed 's/^/         | /' >&2
  fi
  AFTER="$(last_run_log)"
  info "$AFTER"
  OUTSTANDING="$(applied_ids "$AFTER")"
  if [ "$OUTSTANDING" = "claude_accounts,claude_trust,claude_onboarding,claude_agents_view" ]; then
    pass "the sign-in and the three tasks that depend on it are all that is outstanding"
  else
    fail "expected only claude_accounts,claude_trust,claude_onboarding,claude_agents_view outstanding — got [$OUTSTANDING]"
  fi
fi

step "Naming a repository on the command line"

# `--repo` is how an unattended run, a script, or this suite says which
# repository to work on without a prompt. Asserted against the repository already
# checked out, so it proves the flag's path — parse, record, and provision *that*
# repository — without paying for a second clone on a hosted runner.
#
# The prompt itself is not reachable from here and is not meant to be: `Ui::ask`
# answers `None` with no terminal, which is the documented behaviour this run
# relies on everywhere else. Its rules are unit-tested in `repo::pick`.
if [ "$SIGN_IN" = done ]; then
  if REPO_RUN="$(riabuild --repo "$E2E_REPO_SLUG" --no-shell 2>&1)"; then
    pass "a run naming its repository exits 0"
  else
    fail "a run naming its repository did not exit 0"
    printf '%s\n' "$REPO_RUN" | sed 's/^/         | /' >&2
  fi
  check_contains "and says which repository it is working on" \
    "$REPO_RUN" "$E2E_REPO_SLUG"
  NAMED="$(last_run_log)"
  check_contains "and still applies nothing, because it is the same repository" \
    "$NAMED" "applied=[]"
else
  info "a named repository was not provisioned: the run stopped before the sign-in finished"
fi

# Outside that gate on purpose. A repository nobody could clone is refused by
# `main::remember_repo`, which runs before anything is provisioned and needs no
# session — so this is the one assertion here that a run with an unfinished
# sign-in can still make, and the whole block was reporting "not exercised" in CI
# while it sat inside.
#
# The message is asserted, not merely the exit code: without a session every
# invocation exits non-zero, so a status check alone would pass for the wrong
# reason on exactly the run this is here to cover.
REFUSED="$(riabuild --repo "Clubria/.." --no-shell 2>&1 || true)"
check_contains "a repository name that escapes its directory is refused, by name" \
  "$REFUSED" "--repo"

step "Drift is detected and repaired"

rm -f "$RIA_HOME/bin/pnpm"
if [ "$SIGN_IN" = done ]; then
  riabuild --no-shell >/dev/null 2>&1 || fail "the repair run did not exit 0"
  REPAIR="$(last_run_log)"
  info "$REPAIR"
  check "pnpm is back" test -x "$RIA_HOME/bin/pnpm"
  check_contains "the toolchain was repaired" "$REPAIR" "toolchain"
  # claude_accounts depends on toolchain, so it re-running is the dependency
  # cascade working. login, github_cli and project depend on nothing that moved.
  for untouched in login github_cli project; do
    check_missing "$untouched was left alone" "$REPAIR" "$untouched"
  done
else
  # The repair itself is unaffected: `toolchain` runs long before the sign-in, so
  # a run that dies at the sign-in still puts pnpm back. What is unavailable is
  # the run log, so the two halves are asserted separately — the repair on the
  # machine, where it actually matters, and "nothing else moved" through the
  # `--check` that follows it.
  DRIFT="$(riabuild --check --no-shell 2>&1)" || true
  check_contains "the drift is seen" "$DRIFT" "Node and pnpm would run"
  riabuild --no-shell >/dev/null 2>&1 || true
  check "pnpm is back" test -x "$RIA_HOME/bin/pnpm"
  REPAIRED="$(riabuild --check --no-shell 2>&1)" || fail "a --check after the repair did not exit 0"
  REMAINING="$(applied_ids "$(last_run_log)")"
  # Back to exactly the four the sign-in blocks: the toolchain was repaired, and
  # nothing that depends on it was left needing a re-run. login, github_cli and
  # project depend on nothing that moved and must not appear either.
  if [ "$REMAINING" = "claude_accounts,claude_trust,claude_onboarding,claude_agents_view" ]; then
    pass "the toolchain is correct again and nothing else was disturbed"
  else
    fail "after the repair, expected only claude_accounts,claude_trust,claude_onboarding,claude_agents_view — got [$REMAINING]"
    printf '%s\n' "$REPAIRED" | sed 's/^/         | /' >&2
  fi
fi

# ---------------------------------------------------------------------------
# 13. The environment a developer actually lands in
# ---------------------------------------------------------------------------

step "The environment"

ENV_OUT="$(riabuild env 2>&1)"
check_contains "PATH gets riabuild's bin directory" "$ENV_OUT" "$RIA_HOME/bin"
check_contains "PATH gets riabuild's Node" "$ENV_OUT" "$RIA_HOME/node/$NODE_VERSION/bin"
check_contains "the shell is marked as riabuild's" "$ENV_OUT" "RIABUILD_SHELL='1'"
# Deliberately absent. The launchers in ~/.riabuild/bin each set their own
# account's CLAUDE_CONFIG_DIR; an exported one would override every launcher at
# once and quietly make all nine accounts share a config directory. One
# mechanism, not two.
check_missing "the environment pins no single account" "$ENV_OUT" "CLAUDE_CONFIG_DIR"

# `riabuild shell` spawns the developer's real shell. Feeding it a command on
# stdin runs the actual handoff — rcfile generation, ZDOTDIR, PATH — rather than
# asserting on the strings riabuild would have written.
for sh in zsh bash; do
  if command -v "$sh" >/dev/null 2>&1; then
    # A `VAR=value func` prefix, not `env` — `riabuild` here is a shell function
    # that redirects HOME, and `env` can only run a real executable.
    SHELL_OUT="$(printf 'command -v node pnpm claude\nexit\n' \
      | SHELL="$(command -v "$sh")" riabuild shell 2>&1 || true)"
    # pnpm and claude are shims in ~/.riabuild/bin; node comes straight out of
    # the tarball directory riabuild owns. Both are on the PATH it hands over.
    check_contains "$sh: node resolves inside the environment" "$SHELL_OUT" \
      "$RIA_HOME/node/$NODE_VERSION/bin/node"
    check_contains "$sh: pnpm resolves inside the environment" "$SHELL_OUT" "$RIA_HOME/bin/pnpm"
    # Whichever path this run took, `claude` must come out of the tree riabuild
    # owns and nowhere else — a developer's `claude` resolving to something on the
    # machine's own PATH is the failure worth catching. The two answers are
    # different files, so the assertion names the one this run should produce
    # rather than a prefix both would satisfy.
    if [ "$SIGN_IN" = done ]; then
      check_contains "$sh: claude resolves to its launcher" "$SHELL_OUT" "$RIA_HOME/bin/claude"
    else
      check_contains "$sh: claude resolves inside riabuild's Node" "$SHELL_OUT" \
        "$RIA_HOME/node/$NODE_VERSION/bin/claude"
    fi
  fi
done

# Losing this silently destroys a developer's prompt, aliases and history, which
# reads as "riabuild broke my shell".
if [ -f "$RIA_HOME/shell/zsh/.zshrc" ]; then
  check_contains "the generated .zshrc sources the developer's own first" \
    "$(cat "$RIA_HOME/shell/zsh/.zshrc")" 'source "$ZDOTDIR/.zshrc"'
fi

# ---------------------------------------------------------------------------
# 14. The accounts a developer can see and manage
# ---------------------------------------------------------------------------

step "Claude Code accounts"

# `riabuild claude list` is local: no session, no network, no provisioning. It
# is also the only way a developer learns which number to type at the other
# subcommands, so it has to work on a machine that has just been provisioned.
if ! LIST_OUT="$(riabuild claude list 2>&1)"; then
  printf '%s\n' "$LIST_OUT" | sed 's/^/         | /' >&2
  die "riabuild claude list failed."
fi
printf '%s\n' "$LIST_OUT" | sed 's/^/         | /'
check_contains "the account box has its heading" "$LIST_OUT" "Your Claude Code accounts:"
check_contains "account 1 is listed" "$LIST_OUT" "1."
check_contains "account 1 names both its launchers" "$LIST_OUT" "claude-1 / claude"

if [ "$SIGN_IN" = done ]; then
  check_missing "account 1 is not reported as logged out" "$LIST_OUT" "(logged out)"
else
  # The one thing about the three-state read that only a real machine can prove.
  # Asked about a real, freshly created, never-signed-in config directory, real
  # Claude Code answers `loggedIn: false` and riabuild must report *logged out* —
  # not "cannot tell", which is reserved for an answer it genuinely could not
  # read. Collapsing those two is the bug the plan shipped with and the spec
  # forbids: it spends riabuild's ignorance as a browser sign-in on every run of a
  # machine whose state simply cannot be read. Unit tests pin the parse against
  # canned JSON; this pins the JSON.
  check_contains "account 1 is reported as logged out" "$LIST_OUT" "(logged out)"
  check_missing "and not as unreadable" "$LIST_OUT" "cannot tell"
  # Hints are only printed when they would work, so this one appearing is also
  # the assertion that riabuild knows which account needs a login.
  check_contains "the box offers the command that fixes it" "$LIST_OUT" "claude-1 auth login"
fi

# ---------------------------------------------------------------------------
# 15. CLAUDE_CONFIG_DIR — undocumented, therefore only true while tested
# ---------------------------------------------------------------------------

step "CLAUDE_CONFIG_DIR still redirects Claude Code"

# Resolved through the PATH riabuild hands the developer rather than guessed at
# a fixed location: whether Claude Code came from riabuild's own npm or was
# already on the machine, the environment is what decides which one a developer
# actually gets, and that is the one worth pinning.
RIABUILD_PATH="$(printf '%s' "$ENV_OUT" | sed -n "s/^export PATH='\(.*\)'$/\1/p" | head -1)"
CLAUDE_BIN="$(PATH="$RIABUILD_PATH" command -v claude 2>/dev/null || true)"
if [ -n "$CLAUDE_BIN" ] && [ -x "$CLAUDE_BIN" ]; then
  info "claude: $CLAUDE_BIN"
  PIN_DIR="$SCRATCH/claude-pin"
  PIN_HOME="$SCRATCH/claude-pin-home"
  mkdir -p "$PIN_DIR" "$PIN_HOME"
  # `config ls` rather than `--version`: reading configuration is what forces
  # Claude Code to decide where its configuration lives, and `--version` answers
  # without touching disk at all.
  env HOME="$PIN_HOME" CLAUDE_CONFIG_DIR="$PIN_DIR" "$CLAUDE_BIN" config ls >/dev/null 2>&1 || true
  if [ -f "$PIN_DIR/.claude.json" ]; then
    pass "Claude Code kept its configuration in CLAUDE_CONFIG_DIR"
    check_missing "and left the home directory alone" \
      "$(ls -A "$PIN_HOME" 2>/dev/null || true)" ".claude"
  else
    # Not a failure of riabuild — a change in Claude Code. Said plainly so
    # whoever reads this knows which repository to go and look at.
    # A failure, not a note. CLAUDE_CONFIG_DIR is undocumented, every per-account
    # launcher depends on it entirely, and an upstream change has to surface
    # here rather than as every developer's accounts quietly merging into one.
    fail "Claude Code did not keep its configuration in CLAUDE_CONFIG_DIR — the per-account launchers' isolation is gone"
  fi
else
  fail "no claude on the PATH riabuild provides — the per-account launchers would not work"
fi

# ---------------------------------------------------------------------------
# 16. Signing out
# ---------------------------------------------------------------------------

if [ "$PLATFORM" = macos ]; then
  step "Signing out"
  riabuild logout >/dev/null 2>&1 || fail "logout did not exit 0"
  # Same redirected HOME as everywhere else — asking with the real one would
  # search a different keychain list and report success for the wrong reason.
  check_missing "the token is gone from the Keychain" \
    "$(keychain find-generic-password -s com.clubria.riabuild -a session-token -w 2>&1 || true)" \
    "$SESSION_TOKEN"
  # Must not start a sign-in, and must not fail: `--check` reports, it never
  # applies. An expired session is the single most likely reason someone runs
  # this, so it has to answer rather than refuse.
  #
  # The output is captured rather than discarded: the first time this failed it
  # printed nothing, and the cause — a task asking the server unauthenticated —
  # took a code read to find rather than a glance at the log.
  if SIGNED_OUT="$(riabuild --check --no-shell 2>&1)"; then
    pass "a signed-out --check still reports"
  else
    fail "a signed-out --check did not exit 0"
    printf '%s\n' "$SIGNED_OUT" | sed 's/^/         | /' >&2
  fi
fi

# ---------------------------------------------------------------------------

printf '\n'
if [ "$FAILURES" -eq 0 ]; then
  # Said differently on the two paths. A green run that never signed anybody in
  # has not been end to end, and reporting that it has is how a known gap becomes
  # a forgotten one.
  if [ "$SIGN_IN" = done ]; then
    printf '\033[32mriabuild works end to end.\033[0m\n'
  else
    printf '\033[32mriabuild works end to end, up to the Claude Code sign-in nobody here can finish.\033[0m\n'
    printf '\033[2msee "The one step CI cannot finish" in e2e/README.md\033[0m\n'
  fi
  exit 0
fi
printf '\033[31m%d assertion(s) failed.\033[0m\n' "$FAILURES"
exit 1
