# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

# ---------------------------------------------------------------------------
# 8. Build
# ---------------------------------------------------------------------------

step "Build"

if [ -n "${RIABUILD_BIN:-}" ]; then
  RIABUILD="$RIABUILD_BIN"
else
  # No RIABUILD_VERSION: the binary reports the 9999.0.0-dev sentinel, which
  # clears minCliVersion and sits above latestCliVersion, so the run under test
  # never upgrades itself out from under the assertions.
  (cd "$REPO/riabuild-cli" && cargo build --locked)
  RIABUILD="$REPO/riabuild-cli/target/debug/riabuild"
fi
[ -x "$RIABUILD" ] || die "no riabuild binary at $RIABUILD"
info "binary: $RIABUILD ($("$RIABUILD" --version))"

