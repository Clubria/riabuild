# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

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

