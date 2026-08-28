# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

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
# The directories behind those accounts
# ---------------------------------------------------------------------------

step "riabuild paths"

# Local for the same reasons `claude list` is — no session, no network — and
# asserted here rather than in a unit test because the whole value of the
# command is that the path it prints is the directory that is *really* there.
# Unit tests pin the layout against a tempdir; only a provisioned machine can
# disagree with it.
if ! PATHS_OUT="$(riabuild paths 2>&1)"; then
  printf '%s\n' "$PATHS_OUT" | sed 's/^/         | /' >&2
  die "riabuild paths failed."
fi
printf '%s\n' "$PATHS_OUT" | sed 's/^/         | /'
check_contains "the variable that points Claude Code at an account is named" \
  "$PATHS_OUT" "CLAUDE_CONFIG_DIR"
# The path itself, not the heading: this is the assertion that the directory
# riabuild prints is the one `11-machine.sh` found on disk and the one the
# launcher execs against.
check_contains "account 1's config directory is the one on this machine" \
  "$PATHS_OUT" "$RIA_HOME/claude/$CLAUDE_ACCOUNT"
check_contains "the Codex home of the first profile is named" \
  "$PATHS_OUT" "$RIA_HOME/codex/1"
check_contains "the Grok Build home of the first profile is named" \
  "$PATHS_OUT" "$RIA_HOME/grok/1"
