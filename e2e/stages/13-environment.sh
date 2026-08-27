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
# Each harness's config directory, named rather than left to a fallback. Unset,
# every Claude Code, Codex and Grok Build reached by any route other than a
# launcher — an absolute path, an editor extension, a hook that reads the
# variable to find the config it edits — uses ~/.claude, ~/.codex or ~/.grok,
# the three directories riabuild does not manage. The numbered launchers still
# export their own over this one, which is what keeps the nine apart.
check_contains "the environment names Claude Code's config directory" "$ENV_OUT" \
  "CLAUDE_CONFIG_DIR='$RIA_HOME/claude/$CLAUDE_ACCOUNT'"
check_contains "the environment names Codex's config directory" "$ENV_OUT" \
  "CODEX_HOME='$RIA_HOME/codex/1'"
check_contains "the environment names Grok Build's config directory" "$ENV_OUT" \
  "GROK_HOME='$RIA_HOME/grok/1'"

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
    # Whichever path this run took, `claude` must be riabuild's *launcher* — the
    # only thing that pins an account's CLAUDE_CONFIG_DIR and layers org policy
    # over it. A developer's `claude` resolving to something on the machine's own
    # PATH is the obvious failure to catch; resolving to the raw binary in
    # riabuild's Node is the quieter one, because it works, and shares one
    # unnumbered account between every session on the laptop.
    #
    # This used to expect the raw binary on a run that stopped at the sign-in,
    # and that was not a second correct answer — it was a machine with no
    # launchers, because `provision` short-circuited on the first failed task
    # before the step that writes them. `provision::after_the_tasks` writes them
    # whatever the tasks did, so there is one answer again.
    check_contains "$sh: claude resolves to its launcher" "$SHELL_OUT" "$RIA_HOME/bin/claude"
  fi
done

# Losing this silently destroys a developer's prompt, aliases and history, which
# reads as "riabuild broke my shell".
if [ -f "$RIA_HOME/shell/zsh/.zshrc" ]; then
  check_contains "the generated .zshrc sources the developer's own first" \
    "$(cat "$RIA_HOME/shell/zsh/.zshrc")" 'source "$ZDOTDIR/.zshrc"'
fi

