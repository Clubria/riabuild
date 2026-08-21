# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

# ---------------------------------------------------------------------------
# 1. Preflight
# ---------------------------------------------------------------------------

step "Preflight"

case "$(uname -s)" in
  Darwin) PLATFORM=macos ;;
  # Linux runs everything except the Keychain assertions, so the flow can be
  # debugged without a Mac. The macOS runner is what makes the run authoritative.
  Linux) PLATFORM=linux ;;
  *) die "riabuild targets macOS; this is $(uname -s)." ;;
esac
info "platform: $PLATFORM"

for tool in cargo node npx pnpm gh git curl python3; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is not installed."
done

# `gh` and `infisical` are riabuild's to install on both platforms now, so
# nothing has to be staged for it — the tasks that fetch them are part of what
# this run is testing.

if [ -z "${E2E_GITHUB_TOKEN:-}" ]; then
  die "E2E_GITHUB_TOKEN is not set.

The end-to-end run needs a GitHub token belonging to a *user* who is an active
member of the org, because riabuild checks membership from both sides:

  - the CLI's github_cli task runs \`gh api /user/memberships/orgs/<org>\`
  - riabuild-web re-verifies membership before brokering any secret

Actions' built-in GITHUB_TOKEN is an installation token, not a user, and gets a
403 from both regardless of permissions. Create a fine-grained PAT with
Organization permissions -> Members: Read and store it as the E2E_GITHUB_TOKEN
repository secret."
fi

# gh reads GH_TOKEN from the environment, which keeps this out of the runner's
# gh config entirely — nothing to write, nothing to clean up, and no chance of
# picking up an ambient login that would make a green run meaningless.
export GH_TOKEN="$E2E_GITHUB_TOKEN"

