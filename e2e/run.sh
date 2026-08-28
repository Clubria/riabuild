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
#   E2E_GITHUB_TOKEN=<token> e2e/run.sh --only 13
#   e2e/run.sh --list
#
# Arguments:
#   --only <spec>     run these stages and the stages whose state they read.
#                     <spec> is a comma-separated list of ids and ranges:
#                     `13`, `11,13`, `3-10,13`. Read "The stages" below before
#                     believing a bare `--only 13` runs one stage — it does
#                     not, and it prints the ones it had to add.
#   --list            print the stage table and exit, running nothing.
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

# The four `# shellcheck disable=SC2034`s below say the same thing four times:
# the reader of the constant is in a stage file, and shellcheck cannot see
# across a `.` whose path it cannot resolve. They are the price of the split and
# not a hint that anything is dead — `--list` and the stage table are what keep
# that honest.

# Cloned instead of the private repo: it exercises the identical clone,
# origin-verification and repo_status paths, in seconds, without putting a
# checkout of the product codebase on a hosted runner. `check()` compares the
# remote against whatever the server says, so the slug being a stand-in is
# invisible to every line of code under test.
E2E_REPO_SLUG="${E2E_REPO_SLUG:-Clubria/riabuild}"
# shellcheck disable=SC2034  # read by stages 07 and 11
E2E_REPO_NAME="${E2E_REPO_SLUG##*/}"

# Below the `9999.0.0-dev` a local build reports, so the run under test never
# decides it is out of date and replaces its own binary with `brew upgrade`.
# shellcheck disable=SC2034  # read by stage 07
E2E_MIN_CLI_VERSION="2026.01.01"
# shellcheck disable=SC2034  # read by stage 07
E2E_LATEST_CLI_VERSION="2026.01.01"

# Distinctive enough that finding it in org-settings.json proves the file came
# from this deployment rather than from a developer's real cache.
# shellcheck disable=SC2034  # read by stage 07
E2E_CLAUDE_SETTINGS='{"env":{"CLUBRIA_E2E":"1"},"permissions":{"deny":["Read(./.env.dev)","Read(./.env.staging)"]}}'

# ---------------------------------------------------------------------------
# Talking to /api/v1 directly
# ---------------------------------------------------------------------------
#
# Two stages call the API with `curl` rather than through riabuild — stage 05
# waits for `/api/v1/me` to answer at all, and stage 07 checks that the session
# it seeded authenticates. Both have to send the version header, and the reason
# is a decision `guard()` makes before it looks at anything else.
#
# `x-riabuild-cli-version` is **not** optional from the server's side. An absent
# header used to return early out of `enforceMinVersion`, which made the version
# floor opt-in from the client: anything that simply did not send one sailed past
# `minCliVersion` on every route. `convex/lib/guard.ts` now reads a missing
# header as version `0`, so a bare `curl` is refused `409 cli_too_old` *before*
# authentication is reached — and a probe waiting for `401` waits out its whole
# timeout and then reports the backend as never having answered.
#
# `9999.0.0` is above every floor this run can meet: the `0.1.0` a fresh
# anonymous deployment falls back to with no `orgConfig` row (stages 05 and 06,
# before anything is seeded) and the E2E_MIN_CLI_VERSION stage 07 seeds. It is
# also what a local build reports and what `convex/testing.fixtures.ts` uses, so
# the harness is not inventing a version of its own.
E2E_CLI_VERSION="9999.0.0"

# One caller for every direct `/api/v1` request, so the header cannot be sent by
# the poll and forgotten by the assertion after it. Anything curl takes goes
# through here.
api_curl() {
  curl -s -H "x-riabuild-cli-version: $E2E_CLI_VERSION" "$@"
}

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------

# `step`, `info`, `pass`, `fail`, `die`, `check`, `check_contains` and
# `check_missing`, with `STEP` and `FAILURES` behind them. Sourced rather than
# spelled out here because every stage file below counts in the same words, and
# a suite whose verdicts are written two ways ends up meaning two things by
# them.
# shellcheck source=lib/assert.sh
. "$REPO/e2e/lib/assert.sh"

# ---------------------------------------------------------------------------
# The stages
# ---------------------------------------------------------------------------
#
# The sixteen sections this run is made of, one file each, under `e2e/stages/`.
# They are **sourced** into this shell, in order, and that is load-bearing: each
# one reads variables and functions the stages before it left behind — SCRATCH,
# API_URL, the `riabuild` wrapper that redirects HOME — and a subshell would
# lose every one of them.
#
# With no arguments all sixteen are sourced in this order and the run is the run
# it has always been.
#
# `--only` is for the other case: changing one assertion in stage 13 and wanting
# to see it again without the fifteen sections in front of it. What it can
# honestly give is less than that, and the reason is worth stating here rather
# than leaving to be discovered.
#
# **There is no state to resume from.** SCRATCH is a fresh `mktemp -d` and
# teardown deletes it; the backend, the stand-in, the seeded session and the
# provisioned machine all live and die inside one process. So `--only 13` cannot
# mean "against the machine yesterday's run built" — that machine is gone, and a
# flag that pretended otherwise would assert against an empty directory and go
# green. That is the exact failure this suite exists to prevent, so `--only`
# does not offer it.
#
# What `--only` means instead: **run these stages and the stages they read state
# from**. The fourth column of the table below names, for each stage, the stages
# whose variables and files it uses. `--only` takes the transitive closure of
# that, adds the two bootstrap stages, and runs the result in the original
# order — printing what it added, so the difference between what was asked for
# and what ran is on the screen rather than implied.
#
# A bare `--only 13` therefore runs 01-11 and 13: eleven stages, not one. What
# it skips is 12, 14, 15 and 16 — and stage 12 alone is a second full `riabuild`
# run, a `--check`, and a drift-and-repair cycle. That is a real saving, and an
# honest one.
#
# Stages 01 and 02 are bootstrap and always run. 01 is preflight — the platform,
# the tools, the token — and 02 makes the scratch tree and arms the teardown
# trap. Neither asserts anything about riabuild, nothing else can start without
# them, and so there is nothing there to leave out.
#
# The closure is the belt and the loop below is the braces: before sourcing a
# stage it re-checks that every stage in that stage's fourth column actually ran
# in *this* process, and `die`s naming the missing ones if not. It cannot fire
# while the table and the closure agree — which is the point. It fires the day
# somebody adds a stage and gets its dependencies wrong, instead of the run
# going green against state nobody built.

E2E_STAGE_DIR="$REPO/e2e/stages"

# id | file | what the section is | the stages whose state it reads
stage_table() {
  cat <<'TABLE'
01|01-preflight.sh|Preflight|
02|02-scratch.sh|Scratch space and teardown|
03|03-github-identity.sh|Who the token is|
04|04-infisical-stub.sh|The Infisical stand-in|
05|05-backend.sh|The backend|
06|06-deployment-config.sh|Deployment configuration|03 04 05
07|07-seed.sh|Seed|03 05 06
08|08-build.sh|Build|
09|09-session-token.sh|The session token, where riabuild expects to find it|07
10|10-run-riabuild.sh|Run riabuild|02 05 08 09
11|11-machine.sh|What it did to the machine|04 10
12|12-invariant.sh|The invariant the whole task engine rests on|10
13|13-environment.sh|The environment a developer actually lands in|04 10 11
14|14-claude-accounts.sh|The accounts a developer can see and manage|10 11
15|15-claude-config-dir.sh|CLAUDE_CONFIG_DIR still redirects Claude Code|13
16|16-signing-out.sh|Signing out|09 10
TABLE
}

E2E_ALL_STAGES="01 02 03 04 05 06 07 08 09 10 11 12 13 14 15 16"
E2E_BOOTSTRAP="01 02"

# No associative arrays anywhere below: macOS ships bash 3.2, and that is the
# shell this suite has to keep working in.
stage_field() { stage_table | awk -F'|' -v id="$1" -v n="$2" '$1 == id { print $n }'; }
stage_file()  { stage_field "$1" 2; }
stage_name()  { stage_field "$1" 3; }
stage_deps()  { stage_field "$1" 4; }

# Membership in a space-separated set, spelled so bash 3.2 can do it.
stage_in_set() {
  case " $2 " in
    *" $1 "*) return 0 ;;
  esac
  return 1
}

stage_usage() {
  printf 'usage: e2e/run.sh [--only <spec>] [--list]\n\n'
  printf '  --only <spec>  run these stages and the stages they read state from.\n'
  printf '                 <spec> is ids and ranges: 13, or 11,13, or 3-10,13.\n'
  printf '  --list         print the stage table and exit.\n\n'
  printf 'With no arguments every stage runs, in order. The header of this file\n'
  printf 'says why --only always runs more stages than the ones you named.\n'
}

stage_list() {
  local id deps
  printf '%-6s %-56s %s\n' stage section 'reads state from'
  for id in $E2E_ALL_STAGES; do
    deps="$(stage_deps "$id")"
    if stage_in_set "$id" "$E2E_BOOTSTRAP"; then
      deps="(bootstrap — always runs)"
    elif [ -z "$deps" ]; then
      deps="—"
    fi
    printf '%-6s %-56s %s\n' "$id" "$(stage_name "$id")" "$deps"
  done
}

# One id, normalised to the two digits the table is keyed by. Leading zeros come
# off by hand rather than through `printf '%02d'`, which reads `08` as a bad
# octal literal and would fail the run on a stage id somebody typed reasonably.
stage_normalise() {
  local raw="$1" n
  case "$raw" in
    ''|*[!0-9]*) die "--only: '$raw' is not a stage number. Try --list." ;;
  esac
  n="${raw#"${raw%%[!0]*}"}"
  [ -n "$n" ] || n=0
  if [ "$n" -lt 1 ] || [ "$n" -gt 16 ]; then
    die "--only: there is no stage $raw. The stages are 01 to 16 — try --list."
  fi
  printf '%02d' "$n"
}

# `13`, `11,13`, `3-10,13` -> the ids they name, one per line, unresolved.
stage_expand() {
  local spec item lo hi n
  spec="$(printf '%s' "$1" | tr ',' ' ')"
  [ -n "$(printf '%s' "$spec" | tr -d ' ')" ] || die "--only needs a stage list, e.g. --only 13"
  for item in $spec; do
    case "$item" in
      *-*)
        lo="$(stage_normalise "${item%%-*}")"
        hi="$(stage_normalise "${item##*-}")"
        [ "$lo" -le "$hi" ] || die "--only: the range '$item' runs backwards."
        n="$lo"
        while [ "$n" -le "$hi" ]; do
          printf '%02d\n' "$n"
          n=$((n + 1))
        done
        ;;
      *)
        stage_normalise "$item"
        printf '\n'
        ;;
    esac
  done
}

# A stage and everything underneath it, transitively.
STAGE_SELECTED=""
stage_select() {
  local id="$1" dep
  if stage_in_set "$id" "$STAGE_SELECTED"; then return 0; fi
  STAGE_SELECTED="$STAGE_SELECTED $id"
  for dep in $(stage_deps "$id"); do
    stage_select "$dep"
  done
}

E2E_ONLY=""
while [ $# -gt 0 ]; do
  case "$1" in
    --only)
      [ $# -ge 2 ] || die "--only needs a stage list, e.g. --only 13"
      E2E_ONLY="$2"
      shift 2
      ;;
    --only=*) E2E_ONLY="${1#--only=}"; shift ;;
    --list) stage_list; exit 0 ;;
    -h|--help) stage_usage; exit 0 ;;
    *)
      stage_usage >&2
      die "unknown argument: $1"
      ;;
  esac
done

# Every stage in the table has a file, and every file in the directory is in the
# table. The second half is the one that earns its place: a stage file nothing
# sources is a file full of assertions that quietly stopped running, which is
# the failure this whole arrangement exists to make impossible.
for _id in $E2E_ALL_STAGES; do
  [ -f "$E2E_STAGE_DIR/$(stage_file "$_id")" ] \
    || die "stage $_id is in the table but $E2E_STAGE_DIR/$(stage_file "$_id") is not there."
done
for _file in "$E2E_STAGE_DIR"/*.sh; do
  _base="${_file##*/}"
  stage_table | grep -qF "|$_base|" \
    || die "e2e/stages/$_base is not in the stage table, so nothing would ever source it."
done

# Expanded once and kept, so a bad spec is refused in one sentence rather than
# in one per place that needed the list.
STAGE_ASKED=""
if [ -n "$E2E_ONLY" ]; then
  STAGE_ASKED="$(stage_expand "$E2E_ONLY" | tr '\n' ' ')" || exit 1
  for _id in $STAGE_ASKED; do stage_select "$_id"; done
  for _id in $E2E_BOOTSTRAP; do stage_select "$_id"; done
else
  STAGE_SELECTED="$E2E_ALL_STAGES"
fi

# Only ever printed on the --only path: a run with no arguments prints exactly
# what it always printed, and this would be the first line to break that.
if [ -n "$E2E_ONLY" ]; then
  _running=""
  _added=""
  for _id in $E2E_ALL_STAGES; do
    if ! stage_in_set "$_id" "$STAGE_SELECTED"; then continue; fi
    _running="$_running $_id"
    if ! stage_in_set "$_id" "$STAGE_ASKED"; then _added="$_added $_id"; fi
  done
  printf '\033[1mstages:\033[0m%s\n' "$_running"
  if [ -n "$_added" ]; then
    printf '\033[2m--only %s also needs%s: nothing later can be trusted without them\033[0m\n' \
      "$E2E_ONLY" "$_added"
  fi
fi

E2E_STAGES_RAN=""
for _id in $E2E_ALL_STAGES; do
  if ! stage_in_set "$_id" "$STAGE_SELECTED"; then continue; fi
  for _dep in $(stage_deps "$_id"); do
    if ! stage_in_set "$_dep" "$E2E_STAGES_RAN"; then
      die "stage $_id ($(stage_name "$_id")) reads state that stage $_dep builds,
and stage $_dep did not run.

Nothing survives a run — the scratch tree, the backend and the provisioned
machine all go away with the process — so there is no earlier run for stage
$_id to lean on. Refusing, rather than asserting against a machine nobody built."
    fi
  done
  # shellcheck source=/dev/null
  . "$E2E_STAGE_DIR/$(stage_file "$_id")"
  E2E_STAGES_RAN="${E2E_STAGES_RAN:+$E2E_STAGES_RAN }$_id"
done

# ---------------------------------------------------------------------------

printf '\n'
if [ "$FAILURES" -eq 0 ]; then
  # Said differently on the two paths. A green run that never signed anybody in
  # has not been end to end, and reporting that it has is how a known gap becomes
  # a forgotten one.
  #
  # A --only run is that same sentence about a smaller thing: it left stages out,
  # so it cannot claim riabuild works, and claiming it anyway is how somebody
  # reads a green partial run as a green full one.
  if [ -n "$E2E_ONLY" ]; then
    printf '\033[32mno assertion failed in stages %s.\033[0m\n' "$E2E_STAGES_RAN"
    printf '\033[2mthis was --only %s and not the whole suite, so it has not been end to end\033[0m\n' "$E2E_ONLY"
    exit 0
  fi
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
