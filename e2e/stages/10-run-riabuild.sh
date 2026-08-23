# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

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
# THE DRY RUN CLONED NOTHING — asked three ways, none of them against a path
# this script worked out for itself.
#
# It used to be one `test ! -d "$E2E_HOME/…/Clubria/$E2E_REPO_NAME"`, with the
# path rebuilt here from a per-platform `if`. Its own comment admitted the
# problem and then left it: a negative assertion against a mirrored path
# passes when the mirror is stale, so the day `paths::default_project_dir`
# moved, this would have gone on passing while riabuild cloned into a
# directory nobody was looking at. Every part of it that could go stale is now
# either read back out of riabuild's own record or searched for rather than
# named.

# Every checkout riabuild has recorded, one per line, out of the file riabuild
# writes. `repos` is the map the picker keeps, keyed by `owner/repo`;
# `project_path` is the single checkout older riabuilds recorded, read for the
# same reason `read_active_checkout` reads it below.
recorded_checkouts() {
  python3 - "$RIA_HOME/config.json" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        config = json.load(handle)
except (OSError, ValueError):
    raise SystemExit(0)
for path in (config.get("repos") or {}).values():
    print(path)
legacy = config.get("project_path")
if legacy:
    print(legacy)
PY
}

check "the dry run recorded no checkout in config.json" \
  test -z "$(recorded_checkouts)"
# And nothing on disk either, found by looking rather than by guessing where.
# A `.git` anywhere under the scratch home is a clone, and this is the whole
# machine riabuild was given.
check "the dry run cloned nothing anywhere under the scratch home" \
  test -z "$(find "$E2E_HOME" -type d -name .git -print -quit 2>/dev/null)"
# The third way is asked much later, once riabuild has recorded a path and
# there is a real answer to compare against — see "the checkout riabuild
# recorded did not exist before the real run" below. This is its evidence:
# every directory that existed at the end of the dry run.
find "$E2E_HOME" -type d > "$SCRATCH/dirs-after-dry-run" 2>/dev/null || true

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

