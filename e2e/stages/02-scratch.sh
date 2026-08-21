# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

# ---------------------------------------------------------------------------
# 2. Scratch space and teardown
# ---------------------------------------------------------------------------

step "Scratch space"

SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/riabuild-e2e.XXXXXX")"
E2E_HOME="$SCRATCH/home"
mkdir -p "$E2E_HOME"
info "scratch: $SCRATCH"

CONVEX_PID=""
STUB_PID=""
KEYCHAIN=""
SAVED_ENV_LOCAL=""
# Set by stage 05, the only stage that writes riabuild-web/.env.local. Teardown
# will not delete a file this run did not make, and before --only existed there
# was no run that skipped stage 05 for it to matter for.
ENV_LOCAL_OURS=""

# Everything worth reading after a failure, with the two live credentials
# scrubbed. Copied out rather than kept in place because the scratch tree also
# holds the seeded session token, and a CI artifact is a published thing.
save_logs() {
  local out="$REPO/e2e-logs"
  mkdir -p "$out"
  for log in convex stub; do
    [ -f "$SCRATCH/$log.log" ] || continue
    sed -e "s|$E2E_GITHUB_TOKEN|<E2E_GITHUB_TOKEN>|g" \
        -e "s|${SESSION_TOKEN:-__none__}|<SESSION_TOKEN>|g" \
        "$SCRATCH/$log.log" > "$out/$log.log"
  done
  printf 'logs saved to %s\n' "$out"
}

teardown() {
  local status=$?
  [ "$status" -ne 0 ] && save_logs || true
  if [ "${E2E_KEEP:-}" = "1" ]; then
    printf '\nE2E_KEEP=1: leaving %s, backend and stub running.\n' "$SCRATCH"
    return $status
  fi
  printf '\n--- teardown ---\n'
  [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null || true
  # `convex dev` spawns the backend as a child that does not always go with it,
  # so killing the `npx` alone leaves a `convex-local-backend` holding the
  # deployment's port and this run's data directory.
  #
  # This used to be `pkill -f convex-local-backend`, which names no run: it
  # kills the developer's own `pnpm dev`, and it kills the backend belonging to
  # a second worktree's e2e run that happens to be halfway through its
  # assertions. Nothing about the pattern is scoped to *this* invocation, and
  # nothing ever could be — the backend is a shared binary run from a shared
  # path with a command line this script does not choose.
  #
  # The process *group* is the handle that is genuinely ours. `set -m` around
  # the launch (see the Convex step) puts `npx convex dev` in a group of its
  # own, every process it spawns inherits it, and a negative pid signals that
  # group and nothing outside it. Portable to macOS's bash 3.2, which has no
  # `setsid`.
  #
  # TERM first, so the backend closes its database; KILL after a grace, because
  # a backend that will not stop must not leave the port held for the next run.
  # There is deliberately no `pkill` fallback underneath this: an unscoped kill
  # is worse than a leaked process, and a leaked one is visible.
  if [ -n "$CONVEX_PID" ]; then
    kill -TERM -"$CONVEX_PID" 2>/dev/null || kill -TERM "$CONVEX_PID" 2>/dev/null || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      kill -0 -"$CONVEX_PID" 2>/dev/null || break
      sleep 0.5
    done
    kill -KILL -"$CONVEX_PID" 2>/dev/null || true
  fi
  # The keychain and the search list that names it both live inside the scratch
  # tree, so the developer's own keychains were never touched and there is
  # nothing to put back. Deleting it explicitly just tidies up securityd's view.
  [ -n "$KEYCHAIN" ] && env HOME="$E2E_HOME" security delete-keychain "$KEYCHAIN" 2>/dev/null || true
  if [ -n "$SAVED_ENV_LOCAL" ] && [ -f "$SAVED_ENV_LOCAL" ]; then
    cp "$SAVED_ENV_LOCAL" "$REPO/riabuild-web/.env.local"
    printf 'restored riabuild-web/.env.local\n'
  elif [ -n "$ENV_LOCAL_OURS" ]; then
    # Ours, and only ours: it names an anonymous deployment nothing else uses.
    #
    # Guarded on this run having actually written one. A full run always has, so
    # this is the same `rm` it always was; a `--only 8` that never started a
    # backend has not, and deleting the developer's own file there would be the
    # new flag doing damage the suite never did.
    rm -f "$REPO/riabuild-web/.env.local"
  fi
  rm -rf "$SCRATCH"
  return $status
}
trap teardown EXIT

