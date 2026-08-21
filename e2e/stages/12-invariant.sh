# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

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

