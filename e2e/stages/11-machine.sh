# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

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
# Against `$E2E_HOME`, and against every path ngrok itself would choose.
#
# This read `$HOME` until 2026-08-21, and `$HOME` is the *runner's* home:
# `riabuild()` above runs everything with `HOME="$E2E_HOME"`, so the one file
# the whole ngrok design turns on could not have appeared at the path being
# checked however badly riabuild misbehaved. It was not an assertion that
# happened to pass, it was one that could not fail — the shape this suite
# exists to not have.
#
# Both platforms' paths, not just this one's: `ngrok.yml` is written by ngrok
# rather than by riabuild, so the location is ngrok's convention and not
# something riabuild's own platform switch decides. A run on Linux that
# somehow produced the macOS path is still a leaked authtoken, and `.ngrok2`
# is where ngrok 2 put it — a version riabuild does not install, and exactly
# the kind of thing a rewritten launcher could reach for by accident.
for ngrok_config in \
  "$E2E_HOME/Library/Application Support/ngrok/ngrok.yml" \
  "$E2E_HOME/.config/ngrok/ngrok.yml" \
  "$E2E_HOME/.ngrok2/ngrok.yml"; do
  check "riabuild wrote no ngrok config at ${ngrok_config#"$E2E_HOME/"}" \
    test ! -f "$ngrok_config"
done

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
# The Claude Code launchers, on both paths — which they were not until this
# suite's own CI run said so.
#
# `provision` used to write `engine::run_all(…)?`, so the first failed task
# short-circuited the step that writes them: a machine that stopped at the
# sign-in had an account, a config directory, and no `claude` to open it with.
# `provision::after_the_tasks` now lands the launchers whatever the tasks did,
# which is most of what carrying on past a failure was for. The account box's
# own advice on exactly this machine is `claude-1 auth login` — a command that
# has to exist for the advice to be worth printing.
check "the claude launcher is executable" test -x "$RIA_HOME/bin/claude"
check "the first account's launcher is executable" test -x "$RIA_HOME/bin/claude-1"

# The isolation the per-account launchers exist for, asserted the way the nine
# codex and grok launchers already are: each pins one account's config
# directory, and Claude Code keeps sign-ins apart by that directory and nothing
# else. Nine launchers sharing one would be nine names for a single account, and
# every other assertion here would still pass.
check_contains "the claude launcher pins account 1's config directory" \
  "$(cat "$RIA_HOME/bin/claude-1" 2>/dev/null || echo '')" \
  "CLAUDE_CONFIG_DIR=\"$RIA_HOME/claude/$CLAUDE_ACCOUNT\""

# `claude` and `claude-1` are one account under two names, the shape `codex` and
# `grok` already have.
if cmp -s "$RIA_HOME/bin/claude" "$RIA_HOME/bin/claude-1"; then
  pass "the bare claude launcher is the first account"
else
  fail "the bare claude launcher is not claude-1"
fi

check "the checkout is a git repository" test -d "$PROJECT_DIR/.git"
check_contains "the checkout's origin is the repo the server named" \
  "$(git -C "$PROJECT_DIR" remote get-url origin 2>&1)" "$E2E_REPO_NAME"
# The third way of asking "the dry run cloned nothing", and the only one that
# can name the directory that actually matters: this is the path riabuild
# itself recorded, not one this script reconstructed, and the question is
# whether it existed before the real run. A dry run that had created it would
# put it in the snapshot taken at the end of that step.
#
# An empty `$PROJECT_DIR` would make `grep -qxF ''` match every line, so it is
# refused here rather than being read as a pass. The assertion above already
# fails on it; this one must not disagree.
if [ -z "$PROJECT_DIR" ]; then
  fail "riabuild recorded no checkout, so there is no path to ask about"
elif grep -qxF "$PROJECT_DIR" "$SCRATCH/dirs-after-dry-run" 2>/dev/null; then
  fail "the dry run had already created $PROJECT_DIR"
else
  pass "the checkout riabuild recorded did not exist before the real run"
fi

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

