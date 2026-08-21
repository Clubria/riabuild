# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

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

