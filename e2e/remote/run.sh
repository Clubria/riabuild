#!/usr/bin/env bash
# End-to-end proof that `riabuild remote` provisions a real Linux box over a
# real SSH connection, with two developers sharing one Unix account.
#
# What this exercises for real: SSH key generation, the host-key trust
# prompt (answered non-interactively via `--accept-host-key`, never
# weakened), authorising a fresh key onto an account that only trusts an
# existing one (via an ssh-agent — see the comment above `run_as`), asking
# the server its own home directory, and the riabuild-web endpoints a run
# reads — `/api/v1/me`, `/api/v1/org/config` and `/api/v1/org/claude-settings`
# (through a small stand-in — see `stub_web.py`).
#
# TWO PREREQUISITES, both outside this script's control. Neither is hidden,
# and neither is allowed to look like a pass.
#
# 1. A GitHub token belonging to an *active Clubria org member*. Before it
#    touches a server, `riabuild remote` runs `GithubCli` on the laptop
#    (`flow.rs::ensure_local_prerequisites`), whose check re-verifies org
#    membership against real GitHub on purpose — a departed developer must
#    fail here rather than on somebody's server. A repo-scoped
#    `GITHUB_TOKEN` cannot answer `/user/memberships/orgs/Clubria`; it needs
#    a user token, which in CI means a bot account's PAT in a secret. This
#    script checks for one up front and refuses to start without it, rather
#    than discovering it four steps in.
#
# 2. A published release with an `x86_64-unknown-linux-musl` checksum.
#    `install::ensure_riabuild` downloads a real release from real GitHub.
#    The musl *tarball* now ships (release.yml gained a Linux matrix), but
#    the Linux job never appends to `riabuild-$version-checksums.txt` — that
#    file is written only by the macOS job. `ensure_riabuild` fetches the
#    checksums before the tarball and refuses without a digest, which is the
#    right way round to fail: it will not install a binary it cannot verify.
#    Until the Linux job publishes its digests, the run cannot get past the
#    install step.
#
# Because of (2) the assertions at the bottom DO NOT RUN yet.
# This script does not paper over that: it asserts positively, against the
# container and this laptop's own filesystem, that the stages it claims to
# cover actually happened — a key pair on the laptop, a host key pinned, a
# new key in the container's authorized_keys — and it fails if they did not.
# "Stopped at the install step" is only an acceptable outcome once those
# have been proven; on its own it is indistinguishable from "stopped at step
# one", which is exactly how an earlier version of this script reported a
# hang as a success.
#
# WHAT A LINUX/MUSL CHECKSUM ALONE WILL NOT UNBLOCK. An earlier version of
# this comment claimed the assertions would start running the moment
# that asset shipped, with no edit needed here. That is false, and saying so
# is the point of this paragraph — the next step after the install is
# `session::ensure`, and it needs three things this setup does not have:
#
#   a. CLOSED — a keyring is no longer needed. This used to read:
#      `session::ensure` calls `keychain::for_account(…, None)`, which on
#      Linux is `secret-tool` and *errors* when it is missing rather than
#      falling back, so a GitHub ubuntu runner stops here and would need an
#      `apt-get install -y libsecret-tools` (and a session bus, or
#      "`--no-keyring`-equivalent handling", for it to actually store).
#
#      It is the second of those that shipped, and it turned out to matter
#      well beyond this harness: the same "is there a keyring?" test was
#      `which("secret-tool")` at three call sites, which is not the question
#      — libsecret is a client for a D-Bus Secret Service, so the binary is
#      present on plenty of machines with nothing listening. `for_account`
#      now falls back to a 0600 file per server, exactly as the saved SSH
#      password already did. `RIABUILD_TOKEN` still cannot answer this, and
#      still should not: it is this machine's own override, and honouring it
#      here would hand every server the same token.
#   b. `POST` in the stub. `stub_web.py` implements only `do_GET` and
#      `do_DELETE`, so a POST gets BaseHTTPRequestHandler's stock 501.
#   c. A reply to that POST. This item has shrunk twice. It first read "an
#      answer to the loopback browser callback", which #30 made obsolete when
#      `auth::login` stopped opening a local port. It then read "both the
#      device and token endpoints, *and* something to play the approving
#      human" — a stub that has to impersonate a person clicking approve.
#      That is now gone too: signing a server in is one authenticated
#      `POST /api/v1/cli/sessions`, so the stub needs one `do_POST` returning
#      `{token, sessionId, expiresAt}` and no notion of a human at all.
#
# None of that is written yet, on purpose: it is a second piece of work, not
# a line to sneak into this file. It is, however, now small enough to be
# worth doing — closing (b) and (c) is one handler.
#
# WHERE IT ACTUALLY STOPS TODAY, which is further than any of the above
# predicted. v2026.08.10 publishes the musl checksum, so the install now
# completes — and getting there took three fixes, each hidden behind the one
# before it, none of which any test could see because this script had never
# run in CI:
#
#   1. The `gh api` call that resolves the release ran without a token and
#      without `|| true`, so it exited 4 and `set -euo pipefail` ended the
#      script four lines before the fallback that exists for that very case.
#   2. `ssh-copy-id` builds its temporary directory under the developer's
#      `~/.ssh` and fails when there is none. riabuild never created it — a
#      real bug on any laptop that has not used ssh before, which is exactly
#      the machine riabuild claims to be the first thing run on. (Since
#      `authorise` grew its own copy step, riabuild no longer runs
#      `ssh-copy-id` at all, and needs no local `~/.ssh` to install a key.)
#   3. `ensure_matching_binary` compared the *binary* on the server against
#      the *tarball's* digest, which is what a release publishes. Those are
#      never equal, so remote mode could not install on any platform. The
#      unit tests scripted the server's `sha256sum` to answer with the value
#      the assertion was about to compare against, so they agreed with
#      themselves.
#
# It now stops at (b): the CLI POSTs `/api/v1/cli/sessions` and the stub
# answers 501. The `known_gap` check below correctly refuses to forgive that,
# and this paragraph is the reminder — the same role the previous one played,
# now one stage further along.
#
# THE CLIPBOARD CHANNEL IS NOT TESTED HERE. `channel.sh`, beside this file,
# covers it — and runs to the end, because it copies a musl binary onto the
# container instead of installing one and so begins where this script stops.
# What neither script *observes* is remote mode's own channel wiring
# (`src/remote/channel.rs`): the supervisor holding the tunnel up,
# `RIABUILD_CHANNEL_SOCKET` in the `env 'K=V' … '/abs/riabuild'` prefix, and
# the banner line. `channel.sh` stands all three up by hand, so it proves the
# channel works and not that remote mode builds one. That assertion belongs in
# this file, below, once a provisioned server exists to make it against.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
work="$(mktemp -d)"

RIABUILD_BIN="${RIABUILD_BIN:-$repo_root/riabuild-cli/target/release/riabuild}"
STUB_PORT="${STUB_PORT:-8791}"
CONTAINER_PORT="${CONTAINER_PORT:-2222}"
MEMBER_A="11111111-1111-4111-8111-111111111111"
MEMBER_B="22222222-2222-4222-8222-222222222222"

stub_pid=""
agent_pid=""
cleanup() {
  [ -n "$stub_pid" ] && kill "$stub_pid" >/dev/null 2>&1 || true
  [ -n "$agent_pid" ] && kill "$agent_pid" >/dev/null 2>&1 || true
  docker rm -f riabuild-e2e >/dev/null 2>&1 || true
  rm -f "$here/authorized_keys"
  rm -rf "$work"
}
trap cleanup EXIT

echo "== riabuild remote e2e =="

if [ ! -x "$RIABUILD_BIN" ]; then
  echo "RIABUILD_BIN ($RIABUILD_BIN) is not an executable. Build it first:" >&2
  echo "  cd riabuild-cli && cargo build --release --locked" >&2
  exit 1
fi

# The tools this script drives, checked here for the same reason the token is:
# a missing one otherwise surfaces as `docker: command not found` in the
# middle of a build step, or as a stub that never answers and a curl loop that
# quietly times out.
command -v docker >/dev/null 2>&1 || {
  echo "docker is not installed (or not on PATH). This test runs a real sshd" >&2
  echo "in a container; there is no way to fake that half." >&2
  exit 1
}
command -v python3 >/dev/null 2>&1 || {
  echo "python3 is not installed (or not on PATH). It runs stub_web.py, the" >&2
  echo "stand-in for riabuild-web's /api/v1 endpoints." >&2
  exit 1
}
command -v ssh-keygen >/dev/null 2>&1 && command -v ssh-keyscan >/dev/null 2>&1 || {
  echo "ssh-keygen and ssh-keyscan are needed: this script generates the seed" >&2
  echo "key and reads the container's host key fingerprint with them." >&2
  exit 1
}

# Prerequisite 1. Checked here, before a container is built, because the
# failure four steps in is unreadable: riabuild would report a GitHub
# membership problem and nothing would say the test was simply never given a
# credential. `gh auth token` covers a developer running this by hand.
token="${RIABUILD_E2E_GH_TOKEN:-${GH_TOKEN:-}}"
if [ -z "$token" ] && command -v gh >/dev/null 2>&1; then
  token="$(gh auth token 2>/dev/null || true)"
fi
if [ -z "$token" ]; then
  echo "No GitHub token available." >&2
  echo "This test needs one belonging to an active Clubria org member —" >&2
  echo "riabuild re-verifies membership before it touches a server. Set" >&2
  echo "RIABUILD_E2E_GH_TOKEN, or run \`gh auth login\` first." >&2
  exit 1
fi
if ! GH_TOKEN="$token" gh api /user/memberships/orgs/Clubria >/dev/null 2>&1; then
  echo "The available GitHub token cannot read Clubria org membership." >&2
  echo "A repo-scoped GITHUB_TOKEN cannot answer /user/memberships/orgs/*;" >&2
  echo "this needs a user token from an active member of the org." >&2
  exit 1
fi

# One key, two developers: they share the Unix account, which is the point.
# An ssh-agent (rather than `-i`) is what lets `authorise::authorise`'s copy
# step add each developer's *new* riabuild key onto the container without ever
# needing this account's (nonexistent) password. That works because the copy
# is the one `ssh` riabuild runs without `IdentitiesOnly=yes`, so the agent's
# identities are still offered — the same property `ssh-copy-id` relied on
# when this step was still shelling out to it.
ssh-keygen -t ed25519 -N "" -f "$work/seed" -C "riabuild e2e" >/dev/null
cp "$work/seed.pub" "$here/authorized_keys"

eval "$(ssh-agent -s)" >/dev/null
agent_pid="$SSH_AGENT_PID"
ssh-add "$work/seed" >/dev/null 2>&1

echo "-- building the container"
docker build -q -t riabuild-e2e "$here" >/dev/null
docker run -d --name riabuild-e2e -p "$CONTAINER_PORT:22" riabuild-e2e >/dev/null

echo "-- waiting for sshd"
ready=""
for _ in $(seq 1 30); do
  if ssh-keyscan -p "$CONTAINER_PORT" -t ed25519 localhost 2>/dev/null | grep -q ssh-ed25519; then
    ready=1
    break
  fi
  sleep 1
done
if [ -z "$ready" ]; then
  echo "sshd never came up in the container" >&2
  docker logs riabuild-e2e >&2 || true
  exit 1
fi

# The fingerprint answers `--accept-host-key`'s prompt without weakening it:
# a mismatch still fails host_key::trust_host outright.
fingerprint="$(ssh-keyscan -p "$CONTAINER_PORT" -t ed25519 localhost 2>/dev/null \
  | ssh-keygen -lf - | awk '{print $2}')"
if [ -z "$fingerprint" ]; then
  echo "could not read a host key fingerprint from the container" >&2
  exit 1
fi
echo "-- container host key: $fingerprint"

# A real, published release — resolved rather than hardcoded, so this script
# starts exercising a real Linux checksum automatically the day one exists,
# with no edit needed here. Falls back to a pinned, immutable version if
# nothing can answer (no `gh`, no token, offline), which reproduces today's
# known gap deterministically rather than failing to even start.
#
# Three things this line has to do that it did not:
#
# * Pass the token. `gh` on a fresh runner is signed in to nothing, and this
#   repository is private, so an unauthenticated read of its releases cannot
#   work. The token is right here — already validated against the org above.
# * Actually tolerate a failure. `set -o pipefail` is on, so `gh` exiting 4
#   ("please run gh auth login") took the whole script down with it, four
#   lines below a comment promising a fallback. The `if` is what makes that
#   fallback real, and it strips the `v` by expansion rather than with a `sed`
#   at the end of a pipeline, so nothing depends on which member of a pipeline
#   the exit status came from.
# * Believe stdout only when `gh` succeeded. `--jq` prints the API's error
#   body on failure — to *stdout*, which `2>/dev/null` does not touch — so a
#   token that is present but rejected hands back `{ "message": "Bad
#   credentials"… }` as the version. Guarded twice: the `if`, and a shape
#   check, because a version this script goes on to ask a container to install
#   is not a string to take on trust.
#
# None of it could show up until today: the job that runs this was skipped for
# want of RIABUILD_E2E_GH_TOKEN, so a vacuous green tick covered all three.
version=""
if command -v gh >/dev/null 2>&1; then
  if tag="$(GH_TOKEN="$token" gh api repos/Clubria/riabuild/releases/latest \
    --jq .tag_name 2>/dev/null)"; then
    version="${tag#v}"
  fi
  case "$version" in
    [0-9]*) ;;
    *) version="" ;;
  esac
fi
if [ -z "$version" ]; then
  version="2026.08.04"
  echo "-- could not resolve the latest release; pinning to v$version"
else
  echo "-- probing against the real, published riabuild v$version"
fi

cat > "$work/members.json" <<JSON
{
  "test-token-ada": {
    "githubLogin": "ada",
    "memberId": "$MEMBER_A",
    "firstName": "Ada",
    "lastName": "Lovelace",
    "email": "ada@clubria.dev",
    "role": "developer",
    "status": "active"
  },
  "test-token-bob": {
    "githubLogin": "bob",
    "memberId": "$MEMBER_B",
    "firstName": "Bob",
    "lastName": "Bobalooba",
    "email": "bob@clubria.dev",
    "role": "developer",
    "status": "active"
  },
  "__version__": "$version"
}
JSON

# Logged to a file as well as shown, because `known_gap` needs to read what
# the stub was actually asked for. Which *endpoint* returned 501 is the whole
# difference between "this harness has no `do_POST` yet", which is tracked,
# and "riabuild started calling something unexpected", which is a bug — and
# from riabuild's side both look like `replied with HTTP 501`.
# A plain redirect, deliberately not `| tee`: in a pipeline `$!` is the *last*
# element, so `stub_pid` would name `tee` and the teardown below would leave
# python3 holding the port. The log is printed on any failure that is not a
# tracked gap, which is the only time its contents matter.
python3 "$here/stub_web.py" "$STUB_PORT" "$work/members.json" \
  >"$work/stub_web.log" 2>&1 &
stub_pid=$!
for _ in $(seq 1 20); do
  if curl -sf "http://127.0.0.1:$STUB_PORT/api/v1/org/config" >/dev/null 2>&1; then
    break
  fi
  sleep 0.2
done

# `timeout` is not belt-and-braces here: an earlier version of this script sat
# for ten minutes because `gh auth login --web` waits on a person who does not
# exist, and a silent wait is the one failure a CI log cannot diagnose. riabuild
# now refuses that prompt outright when it has no terminal; this bounds anything
# that learns the same trick next.
# `env -u`, not `RIABUILD_ROOT=`. An empty value is not an absent one: it
# reaches `paths::root_for` as `Some("")`, which is refused as "not an absolute
# path", so every run would die at startup on a developer machine that happens
# to export it — a worse failure than the inherited root this is here to stop.
run_as() {                       # run_as <member-id> <login> <token>
  env -u RIABUILD_ROOT \
    HOME="$work/laptop-$2" \
    GH_TOKEN="$token" \
    RIABUILD_API_URL="http://127.0.0.1:$STUB_PORT" \
    RIABUILD_WEB_URL="http://127.0.0.1:$STUB_PORT" \
    RIABUILD_TOKEN="$3" \
    timeout 300 "$RIABUILD_BIN" remote "shared@localhost:$CONTAINER_PORT" \
      --accept-host-key "$fingerprint" --no-shell --quiet
}

in_container() { docker exec riabuild-e2e su - shared -c "$1"; }

echo "-- running as ada (member $MEMBER_A)"
mkdir -p "$work/laptop-ada"
set +e
run_as "$MEMBER_A" ada test-token-ada >"$work/ada.log" 2>&1
ada_status=$?
set -e
cat "$work/ada.log"

# The one failure this script is allowed to exit 0 on — and it has to be
# *that* failure, named. `could not download.*(404|Not Found)` used to be
# enough, which forgave any 404 the install step produced: a wrong asset
# name, a wrong repo, a version that resolved to nothing. Absorbing those
# under a banner that asserts the earlier stages ran is how a real regression
# gets reported as a known gap. Both branches below now require
# `x86_64-unknown-linux-musl` — the target riabuild actually resolved for
# this container — to appear in the message.
known_gap() {
  # Branch 1 is precisely "the asset URL 404s", NOT "the asset is unpublished"
  # — the two are indistinguishable from here, because the URL the CLI asks
  # for and the name this pattern expects are both built from
  # `riabuild_asset_url`. So once a musl asset ships under a name that
  # disagrees with release.yml's packaging step, that mismatch is forgiven as
  # "not published yet". Narrow (the run stops at the keyring gap and fails
  # anyway), but it is the one real bug this branch can absorb.
  grep -qE "could not download.*x86_64-unknown-linux-musl" "$work/ada.log" && return 0
  # Or it is published without a checksum. `Failure` prints its action and
  # its detail on separate lines, so this is two greps rather than one
  # same-line regex that could never match.
  grep -q "missing a checksum for this platform" "$work/ada.log" \
    && grep -q "no checksum for riabuild-.*-x86_64-unknown-linux-musl" "$work/ada.log" \
    && return 0

  # Branch 3, and the one that fires today. v2026.08.10 publishes the musl
  # checksum, so the install completes and the run reaches `session::ensure`,
  # which asks riabuild-web to sign the server in — and `stub_web.py`
  # implements only `do_GET`/`do_DELETE`, so that POST gets
  # BaseHTTPRequestHandler's stock 501. That is item (b) in this file's
  # header: a limitation of this harness, not of riabuild.
  #
  # The path is `/api/v1/cli/sessions`, not `/api/v1/cli/device`: a laptop no
  # longer runs a second device-code flow to sign a server in, it asks under
  # its own token. Note the 501 still reads as a 501 — `auth::for_server`
  # rewrites only a 404, because only a 404 means "this dashboard has no such
  # endpoint", and swallowing every status would hide a real outage behind a
  # sentence about deploying.
  #
  # Both greps, and the endpoint named exactly. A bare "501" would forgive any
  # unimplemented method on any path, including one riabuild had started
  # calling by mistake — and the whole purpose of this function is that a
  # non-start must never be mistaken for a tracked gap. When the stub grows a
  # `do_POST`, this branch stops matching on its own and the assertions
  # below start running.
  grep -q "replied with HTTP 501" "$work/ada.log" \
    && grep -q "POST /api/v1/cli/sessions.* 501" "$work/stub_web.log" 2>/dev/null \
    && return 0

  # There is no branch 4. It used to forgive item (a) — a runner with no
  # libsecret, where `for_account` errored instead of falling back, stopping
  # CI one stage *earlier* than a developer machine with a keyring. That was
  # a real gap in riabuild rather than in this harness, and it is fixed: a
  # machine whose keyring does not answer now keeps a server's session in a
  # 0600 file. So CI and a laptop stop at the same place, branch 3, and this
  # job stops being the one that proves least.
  #
  # Do not restore it. A keyring failure reaching this script again is a
  # regression in `keychain::keyring_answers`, and forgiving it here is how
  # it would ship unnoticed.
  return 1
}

if [ "$ada_status" -eq 124 ]; then
  echo "riabuild remote hung and was killed at 300s. A hang is a failure:" >&2
  echo "something is waiting on input that cannot arrive." >&2
  exit 1
fi

# The stages this script claims to cover, proven rather than inferred. Without
# these, "stopped at the install step" is indistinguishable from "stopped at
# step one" — and an earlier version of this script did report exactly that
# kind of non-start as a success.
assert_reached_the_server() {
  local failed=""
  # A key pair this run generated, on the laptop side (identity::ensure_key).
  # One file per `Remote::hash()` under `<root>/ssh-identities` (paths.rs's
  # `identity_dir`), and the hash is not predictable from here — so glob.
  # `<root>` is `$HOME/.riabuild`, because `run_as` sets no RIABUILD_ROOT.
  ls "$work/laptop-ada/.riabuild/ssh-identities/"*.pub >/dev/null 2>&1 \
    || failed="$failed\n  - no SSH key pair was generated on the laptop"
  # The container's host key, pinned (host_key::trust_host) — in riabuild's
  # own known_hosts, never the developer's `~/.ssh/known_hosts`. That is
  # deliberate (`ssh_options` passes `-F /dev/null` and points
  # `UserKnownHostsFile` at `<root>/ssh/known_hosts`) and identity.rs has a
  # test asserting it, so checking `~/.ssh/known_hosts` here could only ever
  # fail.
  grep -q "\[localhost\]:$CONTAINER_PORT" \
    "$work/laptop-ada/.riabuild/ssh/known_hosts" 2>/dev/null \
    || failed="$failed\n  - the container host key was never pinned"
  # riabuild's own key, added to the account it must reach (authorise). The
  # container starts with exactly one line, the e2e seed key, so a second one
  # is riabuild's.
  #
  # This was `[ "$(… wc -l < ~/.ssh/authorized_keys)" -lt 2 ]`, which could not
  # fail on the input that matters most. With no `authorized_keys` at all the
  # redirect fails, the substitution is empty, and `[ "" -lt 2 ]` is not a
  # false condition — it is a malformed one: `test` writes "integer expression
  # expected" and exits 2. `if` cannot tell those apart, so the branch was
  # skipped and the assertion passed. The worse the input, the more certainly
  # it succeeded — inside the very function whose job is to stop a non-start
  # being forgiven as a known gap.
  #
  # `grep -c .` answers 0 for a missing file instead of erroring, and the
  # `case` demands an actual number, so an unreadable count is a failure
  # rather than a pass. The `|| true` guards only the assignment (a bare
  # command substitution that fails would end the script under `set -e`); it
  # suppresses nothing, because the `case` below does all the asserting.
  keys="$(in_container 'grep -c . ~/.ssh/authorized_keys 2>/dev/null' || true)"
  case "$keys" in
    '' | *[!0-9]*)
      failed="$failed\n  - could not count the container's authorized_keys (got '$keys')"
      ;;
    *)
      [ "$keys" -ge 2 ] \
        || failed="$failed\n  - riabuild never authorised its key on the container"
      ;;
  esac
  if [ -n "$failed" ]; then
    echo "The run did not reach the install step at all:" >&2
    printf "%b\n" "$failed" >&2
    exit 1
  fi
}

if [ "$ada_status" -ne 0 ] && known_gap; then
  assert_reached_the_server
  echo
  # Which gap, in the run's own words rather than a fixed paragraph. The
  # previous version of this banner named the musl checksum unconditionally,
  # so once that shipped it went on announcing a gap that no longer existed
  # while the run was in fact stopping somewhere else entirely.
  if grep -q "replied with HTTP 501" "$work/ada.log"; then
    stopped_at="the sign-in step, one stage past the install"
    because="stub_web.py implements only do_GET and do_DELETE, so the
# device-code POST this release's CLI makes gets a stock 501. That is a
# limitation of this harness, not of riabuild: item (b) in this file's
# header. Closing it needs a do_POST and something to approve the code."
  else
    stopped_at="the binary-install step"
    because="riabuild v$version publishes an x86_64-unknown-linux-musl
# tarball but no checksum for it. Releases from v2026.08.10 onward do
# publish one, so this branch should only fire against an older tag."
  fi
  echo "############################################################"
  echo "# KNOWN GAP, not a regression: remote mode stopped at"
  echo "# $stopped_at."
  echo "#"
  echo "# $because"
  echo "#"
  echo "# Asserted just now, not assumed: a key pair was generated,"
  echo "# the container's host key was pinned, and riabuild's key was"
  echo "# authorised on the container. Those stages ran for real."
  echo "#"
  echo "# The assertions this test names were NOT run."
  echo "# They need a provisioned server, which needs a session, which"
  echo "# needs the sign-in above. Nothing in CI has yet proved the"
  echo "# namespace isolation remote mode rests on."
  echo "#"
  echo "# The clipboard channel is NOT among what went untested here:"
  echo "# channel.sh covers it against this same container and runs to"
  echo "# the end. Remote mode's channel wiring still is."
  echo "############################################################"
  exit 0
fi

if [ "$ada_status" -ne 0 ]; then
  echo "riabuild remote failed for a reason that is not the known gap:" >&2
  # The stub's own log, which is no longer on stdout. Without it an HTTP
  # failure here shows riabuild's side of the exchange and not what the stub
  # was asked for, and those two together are what say whether this is a
  # harness limitation or a real regression.
  echo "--- stub_web ---" >&2
  tail -20 "$work/stub_web.log" >&2 2>/dev/null || true
  exit 1
fi

echo "-- ada's run reached the end: a Linux release exists now. Running the real assertions."

echo "-- running as bob (member $MEMBER_B)"
mkdir -p "$work/laptop-bob"
run_as "$MEMBER_B" bob test-token-bob

# The assertions are labelled by what they check, never numbered, and no count
# of them is stated anywhere above. Both of those are deliberate. A number in
# prose has to be restated everywhere it is mentioned and renumbered for every
# assertion added or removed, so a branch that changes the set collides with
# every other branch that so much as reworded a nearby sentence — which is the
# only conflict this file has ever actually had. The closing banner names what
# ran instead of counting it, which is what a reader needed in the first place.
#
# namespaces: two, each with its own state
in_container "test -f ~/.riabuild-remote/$MEMBER_A/state.json"
in_container "test -f ~/.riabuild-remote/$MEMBER_B/state.json"
# git identity: one per developer, and they differ
in_container "grep -q 'ada@' ~/.riabuild-remote/$MEMBER_A/gitconfig"
in_container "grep -q 'bob@' ~/.riabuild-remote/$MEMBER_B/gitconfig"
# toolchain: one, shared by both
test "$(in_container 'ls ~/.riabuild/node | wc -l')" -eq 1
# checkouts: grouped by developer, not shared
in_container "test -d ~/Clubria/ada && test -d ~/Clubria/bob"
# gh credential: none outlives the sessions that created it
#
# There were six assertions here. The missing one claimed to check that gh
# config is isolated *while a session is live*, by testing that
# `$XDG_RUNTIME_DIR/riabuild-gh-$MEMBER_A` exists — and it ended in
# `|| true`, so it could not fail.
#
# The `|| true` was not laziness, and deleting it alone would have been
# wrong: that assertion directly contradicts this one. Both run here, after
# both `run_as` calls have returned, so after every session is already dead.
# One demanded the directory be present; the next demands it be gone. On a
# correct implementation the pair cannot both hold, so the only way to keep
# them together was to disarm one of them — which quietly turned "six
# assertions" into five and a decoration, printed under a banner that says
# they all passed.
#
# Live-session isolation is not observable from out here at all: riabuild
# creates the runtime directory and reaps it before the process this script
# waits on exits, so there is no moment in between for a shell to look. It
# is covered where it can be, by tests that hold a `GhSession` open and
# assert against it directly:
# `gh_session.rs::opening_a_session_makes_a_private_directory_and_a_marker`
# and `::two_sessions_share_one_sign_in_and_the_last_one_out_wipes_it`.
# This file asserts the half a black box really can see: nothing survives.
#
# Read this one as one-sided, because it is: "no hosts.yml is left" is just as
# true of a run where `gh` was never signed in at all, or where seeding it
# silently did nothing — a failure this branch has actually had. Nothing in
# this script asserts a GitHub credential ever *reached* the container, and
# there is nowhere to look for one: the credential lives in the ephemeral
# runtime directory for the life of the process and is reaped with it, which
# is the property being tested. So this catches a leak and cannot catch an
# absence; the assertions above it are what establish the run did real
# work.
test -z "$(in_container 'find /tmp /run -name hosts.yml 2>/dev/null')"
# org settings: the team's Claude Code settings reached each namespace
#
# The launcher layers this file with `--settings`, and drops the flag entirely
# when it is not there — `claude --settings` on a missing path refuses to
# start — so its absence is a silent downgrade to no org policy rather than an
# error anyone would see. Nothing here had ever looked for it, and it does not
# live where a developer would think to look on a server: `RIABUILD_ROOT`
# points at the namespace, so the file is under `.riabuild-remote/<member>/`
# and never at `~/.riabuild/`.
#
# Grepped for the marker, not merely tested for existence: an empty `{}` is a
# valid file that would satisfy `test -f` and carry no policy at all.
in_container "grep -q CLUBRIA_REMOTE_E2E ~/.riabuild-remote/$MEMBER_A/org-settings.json"
in_container "grep -q CLUBRIA_REMOTE_E2E ~/.riabuild-remote/$MEMBER_B/org-settings.json"

# Names what was checked rather than counting it, and never claims "all
# assertions passed" — that phrasing is how this script once reported a run
# that never reached the server, and the count is what used to make every
# branch that changed the set conflict with every other one.
echo "remote mode e2e passed — separate namespaces, separate git identities,"
echo "one shared toolchain, per-developer checkouts, no gh credential left"
echo "behind, and the team's Claude Code settings in each namespace."
