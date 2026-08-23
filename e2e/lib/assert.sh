# shellcheck shell=bash
#
# The assertion vocabulary the end-to-end suite counts in.
#
# Sourced by e2e/run.sh before any stage. Not executable on its own: it defines
# words, runs nothing, and every one of them writes to stdout in a format the
# CI log is read in.
#
# The contract every stage relies on:
#
#   step        opens a numbered section. The number is a running count of the
#               sections that actually ran, not a stage id — see e2e/run.sh.
#   info        a line of context. Never an assertion; never counted.
#   pass / fail the two verdicts. `fail` is the only thing that moves FAILURES,
#               and FAILURES is the only thing the exit status is computed from.
#   check       a verdict on a command's exit status.
#   check_contains / check_missing
#               a verdict on whether some captured output holds a literal
#               string. On failure the haystack is printed, indented, to
#               stderr — the failure is unreadable without it.
#   die         the run cannot continue and every later assertion would be
#               noise. Not a failed assertion: an abandoned run.
#
# Nothing here may exit 0 on a condition it did not actually observe. The whole
# suite is only worth its runtime if a green line means the thing it names.

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
