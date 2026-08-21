# shellcheck shell=bash
#
# Sourced by e2e/run.sh, in the shell the whole run shares. Not executable:
# every stage reads variables and functions earlier stages left behind, and a
# subshell would lose them. e2e/run.sh names what this one needs.

# ---------------------------------------------------------------------------
# 3. Who the token is
# ---------------------------------------------------------------------------

step "GitHub identity"

E2E_ORG="${E2E_ORG:-Clubria}"

# `gh api` writes an HTTP error *body to stdout* and exits non-zero, and it does
# not apply `--jq` to that body. So `$(gh api … 2>/dev/null || true)` captures
# the error JSON as though it were the field asked for — and the result is
# non-empty, which is precisely what a `[ -n … ]` guard tests. A discarded exit
# status is therefore not a small omission here; it inverts the check.
#
# On 2026-08-17 GitHub returned 503 during a partial outage and `E2E_LOGIN`
# became `{"message": "No server is currently available…"}`. It passed this
# guard, was printed as the developer's name, and brought the run down four
# steps later in the seed with `SyntaxError: JSON5: invalid character 'm'` —
# naming neither GitHub nor this line. The old message could not have been
# right either way: it offered "expired, revoked, or not a user token" for a
# failure that was none of those.
#
# So: take the exit status, keep gh's own reason for the message, and validate
# the shape before anything downstream is handed the value.
if ! E2E_LOGIN="$(gh api /user --jq .login 2>"$SCRATCH/github-identity.err")"; then
  die "E2E_GITHUB_TOKEN could not read /user, so this run cannot start.

It is expired or revoked, it is not a user token, or GitHub is unavailable —
check https://www.githubstatus.com before assuming it is the token.

gh said:
$(cat "$SCRATCH/github-identity.err")
$E2E_LOGIN"
fi

# A GitHub login is letters, digits and hyphens, so anything else is not one,
# whatever gh exited with. This is the strict half: every step after this
# interpolates the value into a Convex argument, a path, or a message, and none
# of them can tell a login from an error that happens to be a string.
if [ -z "$E2E_LOGIN" ]; then
  die "GitHub answered /user with no login at all, so this run cannot start.
The token may not be a user token."
elif [[ ! $E2E_LOGIN =~ ^[A-Za-z0-9-]+$ ]]; then
  die "GitHub answered /user with something that is not a login:

$E2E_LOGIN"
fi
info "token belongs to @$E2E_LOGIN"

# Asserted here rather than discovered six steps later as a confusing task
# failure. Everything after this point assumes the answer is yes.
#
# A failure here is *not* normalised away: 404 is how GitHub says "not a
# member" and 403 is how it says "this token may not ask", so an unreadable
# state is a real answer to report rather than an error to retry. What it must
# not do is report only the permissions remedy, because a 5xx lands here too —
# hence gh's own words below the guidance.
MEMBERSHIP_REASON=""
if ! MEMBERSHIP="$(gh api "/user/memberships/orgs/$E2E_ORG" --jq .state 2>"$SCRATCH/github-membership.err")"; then
  MEMBERSHIP_REASON="$(cat "$SCRATCH/github-membership.err")
$MEMBERSHIP"
  MEMBERSHIP="unreadable"
fi
[ "$MEMBERSHIP" = "active" ] || die "@$E2E_LOGIN is not an active member of $E2E_ORG (state: ${MEMBERSHIP:-unreadable}).

If the state is unreadable, the token is missing organisation read access:
a fine-grained PAT needs Organization permissions -> Members: Read, with
$E2E_ORG as the resource owner, and the org has to approve the token.
It can also mean GitHub is unavailable — check https://www.githubstatus.com.
${MEMBERSHIP_REASON:+
gh said:
$MEMBERSHIP_REASON}"
pass "@$E2E_LOGIN is an active member of $E2E_ORG"

