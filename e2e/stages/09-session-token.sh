# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

# ---------------------------------------------------------------------------
# 9. The session token, where riabuild expects to find it
# ---------------------------------------------------------------------------

step "Session token"

if [ "$PLATFORM" = macos ]; then
  # Every `security` call below runs with the same redirected HOME riabuild will
  # run with, and that is the whole trick.
  #
  # The keychain *search list* is a per-user preference living in
  # ~/Library/Preferences, not a global session setting. Register a keychain
  # against the real home and a process whose HOME points elsewhere cannot see
  # it — `security find-generic-password` searches a list that is simply not
  # there. An earlier version of this set the keychain up with the real HOME,
  # verified the read, passed, and then left riabuild reporting "not signed in"
  # and starting a sign-in nobody was there to approve. The verification passed
  # *because* it ran in the wrong environment.
  KEYCHAIN="$SCRATCH/riabuild-e2e.keychain-db"
  mkdir -p "$E2E_HOME/Library/Preferences"
  keychain() { env HOME="$E2E_HOME" security "$@"; }

  keychain create-keychain -p e2e "$KEYCHAIN"
  keychain set-keychain-settings "$KEYCHAIN"   # no auto-lock
  keychain unlock-keychain -p e2e "$KEYCHAIN"
  # Ours is the only entry: nothing else in this throwaway home needs a
  # keychain, so there is no real search list to save and put back either.
  keychain list-keychains -s "$KEYCHAIN"
  # The keychain is the trailing positional argument — `add-generic-password`
  # has no -k, and passing one makes it fail rather than fall back.
  #
  # -A grants every application access. Without it the first read raises a GUI
  # authorisation prompt, and on a headless runner that is an indefinite hang
  # rather than a failure.
  keychain add-generic-password -U -A \
    -s com.clubria.riabuild -a session-token -w "$SESSION_TOKEN" "$KEYCHAIN"

  # Read it back the way riabuild does, in the environment riabuild does it in.
  STORED="$(keychain find-generic-password -s com.clubria.riabuild -a session-token -w 2>&1 || true)"
  [ "$STORED" = "$SESSION_TOKEN" ] \
    || die "the token did not come back out of the Keychain (got: ${STORED:0:40})"
  pass "token stored in a dedicated Keychain and readable the way riabuild reads it"
else
  # riabuild's own escape hatch for machines with no keyring. It skips
  # keychain.rs, which is why the macOS runner is the authoritative one.
  export RIABUILD_TOKEN="$SESSION_TOKEN"
  info "no Keychain on Linux — using the RIABUILD_TOKEN escape hatch"
fi

