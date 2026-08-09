#!/usr/bin/env bash
# End-to-end proof that `riabuild remote` provisions a real Linux box over a
# real SSH connection, with two developers sharing one Unix account.
#
# What this exercises for real: SSH key generation, the host-key trust
# prompt (answered non-interactively via `--accept-host-key`, never
# weakened), authorising a fresh key onto an account that only trusts an
# existing one (via an ssh-agent — see the comment above `run_as`), asking
# the server its own home directory, and riabuild-web's `/api/v1/me` and
# `/api/v1/org/config` (through a small stand-in — see `stub_web.py`).
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
# Because of (2) the five isolation assertions at the bottom DO NOT RUN yet.
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
# this comment claimed the five assertions would start running the moment
# that asset shipped, with no edit needed here. That is false, and saying so
# is the point of this paragraph — the next step after the install is
# `session::ensure`, and it needs three things this setup does not have:
#
#   a. A keyring. `session::ensure` calls `keychain::for_account(…, None)`,
#      which on Linux is `secret-tool` and *errors* when it is missing
#      ("reading the riabuild token from your keyring") rather than falling
#      back. `RIABUILD_TOKEN` cannot answer this: `for_account` ignores it
#      deliberately, because that variable is this machine's own override
#      and honouring it here would hand every server the same token. A
#      GitHub ubuntu runner has no `secret-tool`, so this needs an
#      `apt-get install -y libsecret-tools` in the job (and a session bus,
#      or `--no-keyring`-equivalent handling, for it to actually store).
#   b. `POST /api/v1/cli/token` in the stub. `stub_web.py` implements only
#      `do_GET` and `do_DELETE`, so a POST gets BaseHTTPRequestHandler's
#      stock 501.
#   c. An answer to the loopback browser callback. `auth::login` opens a
#      local port and waits for the dashboard to redirect a browser back to
#      it. Nothing in this script plays that browser. Adding `do_POST` to
#      the stub is not enough on its own; something has to complete the
#      callback, or `session::ensure` needs a seam this test can enter
#      through instead.
#
# None of that is written yet, on purpose: it is a second piece of work, not
# a line to sneak into this file. When a musl checksum ships, this script
# will get *further* than it does today and then stop at (a) — which the
# `known_gap` check below will correctly refuse to forgive, and that failure
# is the reminder.
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
# An ssh-agent (rather than `-i`) is what lets `authorise::authorise`'s
# `ssh-copy-id` step add each developer's *new* riabuild key onto the
# container without ever needing this account's (nonexistent) password —
# verified by hand against a real container before writing this script:
# `ssh-copy-id` tries every identity an agent offers before it ever falls
# back to a password prompt.
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
version=""
if command -v gh >/dev/null 2>&1; then
  version="$(gh api repos/Clubria/riabuild/releases/latest --jq .tag_name 2>/dev/null | sed 's/^v//')"
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

python3 "$here/stub_web.py" "$STUB_PORT" "$work/members.json" &
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
    && grep -q "no checksum for riabuild-.*-x86_64-unknown-linux-musl" "$work/ada.log"
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
  echo "############################################################"
  echo "# KNOWN GAP, not a regression: remote mode stopped at the"
  echo "# binary-install step. riabuild v$version publishes an"
  echo "# x86_64-unknown-linux-musl tarball but no checksum for it:"
  echo "# release.yml writes the checksums file in its macOS job only."
  echo "#"
  echo "# Asserted just now, not assumed: a key pair was generated,"
  echo "# the container's host key was pinned, and riabuild's key was"
  echo "# authorised on the container. Those stages ran for real."
  echo "#"
  echo "# The five isolation assertions this test names were NOT run:"
  echo "# they need an installed server binary, which does not exist"
  echo "# yet. This is expected until release.yml publishes the"
  echo "# musl digests alongside the musl tarballs."
  echo "#"
  echo "# The clipboard channel is NOT among what went untested here:"
  echo "# channel.sh covers it against this same container and runs to"
  echo "# the end. Remote mode's channel wiring still is."
  echo "############################################################"
  exit 0
fi

if [ "$ada_status" -ne 0 ]; then
  echo "riabuild remote failed for a reason that is not the known gap:" >&2
  exit 1
fi

echo "-- ada's run reached the end: a Linux release exists now. Running the real assertions."

echo "-- running as bob (member $MEMBER_B)"
mkdir -p "$work/laptop-bob"
run_as "$MEMBER_B" bob test-token-bob

# 1. two namespaces, each with its own state
in_container "test -f ~/.riabuild-remote/$MEMBER_A/state.json"
in_container "test -f ~/.riabuild-remote/$MEMBER_B/state.json"
# 2. a git identity per developer, and they differ
in_container "grep -q 'ada@' ~/.riabuild-remote/$MEMBER_A/gitconfig"
in_container "grep -q 'bob@' ~/.riabuild-remote/$MEMBER_B/gitconfig"
# 3. one toolchain for both
test "$(in_container 'ls ~/.riabuild/node | wc -l')" -eq 1
# 4. checkouts grouped by developer, not shared
in_container "test -d ~/Clubria/ada && test -d ~/Clubria/bob"
# 5. no gh configuration outlives the sessions that created it
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
# absence; the four assertions above it are what establish the run did real
# work.
test -z "$(in_container 'find /tmp /run -name hosts.yml 2>/dev/null')"

# Names what was checked rather than claiming "all assertions passed", which
# is how this script once reported a run that never reached the server.
echo "remote mode e2e: five assertions passed — separate namespaces, separate"
echo "git identities, one shared toolchain, per-developer checkouts, and no"
echo "gh credential left behind."
