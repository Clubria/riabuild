# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

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

