#!/usr/bin/env bash
# End-to-end proof that `riabuild remote` provisions a real Linux box over a
# real SSH connection, with two developers sharing one Unix account.
#
# What this exercises for real: SSH key generation, host-key trust held to a
# named fingerprint via `--accept-host-key` rather than pinned on sight,
# authorising a fresh key onto an account that only trusts an
# existing one (via an ssh-agent — see the comment above `run_as`), asking
# the server its own home directory, and the riabuild-web endpoints a run
# reads — `/api/v1/me`, `/api/v1/org/config` and `/api/v1/org/claude-settings`
# (through a small stand-in — see `stub_web.py`).
#
# TWO ACTS, AND THE THING THAT MAKES THEM TWO. `riabuild remote` puts riabuild
# on a server by downloading a *published release* and verifying its digest.
# There is no flag, no environment variable and no seam that points that at a
# local build, and there must not be one: a server binary chosen by anything
# other than a signed release is the server-supplied task manifest the root
# `CLAUDE.md` forbids, wearing a different hat. So in a run of this script the
# laptop half is the code under review and the server half is whatever shipped
# last.
#
# That is only a curiosity until the job is asked to *gate a pull request*, at
# which point it is the same defect as a test that cannot fail: a server-side
# regression this branch introduces cannot turn the job red, and a server-side
# bug this branch fixes cannot turn it green. It was found the second way
# round. v2026.08.21.1's pnpm is linked against `libatomic.so.1`, which no
# stock Linux ships, so the released server stopped on `Node and pnpm (it did
# not take effect)` for three rounds running — while the fix for exactly that
# sat in the branch the job was supposedly gating.
#
# So this script runs twice against the one container, and says which half
# proves what.
#
#   ACT ONE — THE LAPTOP, AND THE INSTALL. `riabuild remote` end to end: SSH,
#     host-key trust held to a named fingerprint, authorising a fresh key,
#     asking the server its home directory, `install::ensure_riabuild` fetching
#     `riabuild-<version>-checksums.txt` and then the tarball from real GitHub
#     and refusing without a digest, signing the server in, and lending it this
#     laptop's GitHub sign-in. All of that is this branch's code and act one
#     judges it.
#
#     What act one cannot judge is what the released binary then does with the
#     task DAG. That is last month's code; nothing in this checkout can move it,
#     and a red tick for it teaches a reader the wrong thing. The boundary is
#     not a paragraph — it is `assert_installed_the_published_release` below,
#     which proves the download-and-verify path ran and put the published bytes
#     on the server. A run that fell over anywhere before that is still fatal,
#     and a run that stopped *after* it is handed to act two rather than
#     forgiven: `known_gap` is untouched and no branch was added to it.
#
#   ACT TWO — THE SERVER, RUNNING THIS BRANCH. The musl build named by
#     `RIABUILD_SERVER_BIN` is copied in — `channel.sh` has done exactly this
#     since it was written, for exactly this reason, and says so as plainly —
#     and the three invocations `flow::connect` composes are then made against
#     it over the same SSH, in the same order: `internal gh-sweep`, `internal
#     seed-github` with the token on stdin, and the `env 'K=V' …
#     '/abs/riabuild' --no-shell` setup run. Each of those is riabuild's own
#     subcommand, not a re-implementation of one.
#
#     Its assertions are the ones a released server cannot satisfy: the log
#     line the run wrote has to name *this* build, pnpm has to answer `-v`
#     through the shim on a container that has no `libatomic.so.1`, and
#     `toolchain` must not appear among the tasks riabuild recorded as failed.
#     Those three fail against v2026.08.21.1 and pass with the change, which is
#     what makes this job a gate rather than a report.
#
#     What act two does not prove is the laptop-to-server handoff — it composes
#     the prefix itself rather than watching remote mode compose it. Act one is
#     what proves that, which is why neither act is a substitute for the other
#     and why both run on every invocation.
#
# THREE PREREQUISITES, all outside this script's control. None is hidden,
# and none is allowed to look like a pass.
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
# 2. A published release the container's platform can install from.
#    `install::ensure_riabuild` downloads a real release from real GitHub,
#    fetches `riabuild-$version-checksums.txt` before the tarball, and
#    refuses without a digest — the right way round to fail: it will not
#    install a binary it cannot verify.
#
#    THIS ONE IS CLOSED, and the paragraph is kept because the shape of it
#    recurs. It read, for a long time and for two versions after it stopped
#    being true, that the Linux job never appended to the checksums file. It
#    does: `release.yml`'s assembly step walks all four targets — both darwin,
#    both musl — and errors if a tarball is missing, and v2026.08.10 was the
#    first release to publish the lot. The install completes.
#
#    Getting there took three fixes, each hidden behind the one before it,
#    none of which any test could see because this script had never run in CI:
#
#      a. The `gh api` call that resolves the release ran without a token and
#         without `|| true`, so it exited 4 and `set -euo pipefail` ended the
#         script four lines before the fallback that exists for that very case.
#      b. `ssh-copy-id` builds its temporary directory under the developer's
#         `~/.ssh` and fails when there is none. riabuild never created it — a
#         real bug on any laptop that has not used ssh before, which is exactly
#         the machine riabuild claims to be the first thing run on. (Since
#         `authorise` grew its own copy step, riabuild no longer runs
#         `ssh-copy-id` at all, and needs no local `~/.ssh` to install a key.)
#      c. `ensure_matching_binary` compared the *binary* on the server against
#         the *tarball's* digest, which is what a release publishes. Those are
#         never equal, so remote mode could not install on any platform. The
#         unit tests scripted the server's `sha256sum` to answer with the value
#         the assertion was about to compare against, so they agreed with
#         themselves.
#
# 3. A musl build of *this* checkout, for act two — `RIABUILD_SERVER_BIN`,
#    the same variable and the same target `channel.sh` takes. Checked up
#    front and named with the `cargo` line that produces it, because the
#    alternative is discovering it after act one has already spent a minute
#    building a container and downloading a release.
#
#    x86_64-unknown-linux-musl and not the host build: the container's glibc
#    is older than a GitHub runner's, so the ordinary `target/release/riabuild`
#    starts and dies. musl is also what `release.yml` ships for Linux, so the
#    binary act two runs is the same *kind* of artifact act one installs, built
#    from the branch instead of from the tag.
#
# This script does not paper over a run that stopped early. It asserts
# positively, against the container and this laptop's own filesystem, that the
# stages it claims to cover actually happened — a key pair on the laptop, a
# host key pinned, a new key in the container's authorized_keys — and it fails
# if they did not. "Stopped early" is only an acceptable outcome once those
# have been proven; on its own it is indistinguishable from "stopped at step
# one", which is exactly how an earlier version of this script reported a
# hang as a success.
#
# WHAT A LINUX/MUSL CHECKSUM ALONE DID NOT UNBLOCK, and what is left of it.
# An old version of this comment promised the assertions would start running
# the moment that asset shipped, with no edit needed here. It was wrong four
# times over, and the list is kept because each entry closing is what moved
# this script one stage further along.
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
#   b. CLOSED — `POST` in the stub. `stub_web.py` implemented only `do_GET`
#      and `do_DELETE`, so `session::ensure`'s `POST /api/v1/cli/sessions`
#      got `BaseHTTPRequestHandler`'s stock 501. That 501 was matched as a
#      tracked gap, and the script exited 0 immediately above the assertion
#      block — so *none of those assertions had ever run once*, under a banner
#      saying which stages had been proven. It now mints a delegated session,
#      and no stock 501 is left anywhere for `known_gap` to lean on.
#   c. CLOSED with (b), and kept only for the shape of it. This item first
#      read "an answer to the loopback browser callback", which #30 made
#      obsolete when `auth::login` stopped opening a local port. It then read
#      "both the device and token endpoints, *and* something to play the
#      approving human". Signing a server in is one authenticated POST under
#      the laptop's own token, so the stub needs no notion of a human at all.
#   d. The server's own riabuild has to be able to reach this stand-in, and
#      nothing riabuild does will point it there. The server runs as
#      `env 'K=V' … '/abs/riabuild'` and `remote::env_prefix` puts four
#      variables in that prefix — none of them `RIABUILD_API_URL`. Left alone,
#      a server talks to `DEFAULT_API_URL`, the real deployment, holding a
#      token this stand-in minted and the real one has never heard of. There
#      is no CLI flag for this and there should not be one, so the answer is
#      on the *container's* side: an `~/.ssh/environment` that sshd applies to
#      every session, and a reverse forward carrying the stub's port in. Both
#      are set up below; both are the harness's own container; neither
#      changes a line of riabuild.
#   e. OPEN, and unbounded from here. Past the sign-in the server runs the
#      whole task DAG — a Node toolchain, `gh`, `infisical`, a real clone,
#      real secrets — and this stand-in answers three GET routes and one POST.
#      Every route it does not implement logs `UNIMPLEMENTED <method> <path>`,
#      and that line, not riabuild's error text, is what `known_gap` reads. So
#      the missing piece is *named* in the banner instead of guessed at.
#      Adding those routes, and an Infisical stand-in the container can reach,
#      is the work that finally makes the block at the bottom run.
#
#      Act two runs into the same wall and is bounded accordingly: it asserts
#      the toolchain, which is upstream of the secrets pull, and prints what
#      the run failed on and never reached rather than pretending to a verdict
#      on either. What act two changes is *whose* code is behind that wall.
#   f. CLOSED, and it is the one that was hiding directly behind (b). The
#      first run to get past the sign-in stopped three tasks in, on
#      `git_credentials`: `gh auth setup-git` reported "unable to find git
#      executable in PATH", because `Dockerfile` installed `openssh-server`
#      and `ca-certificates` and nothing else. That is riabuild correctly
#      refusing a machine it cannot provision, so `known_gap` did not forgive
#      it and must not be taught to — the fix belongs in the container, and
#      `git` is now installed there with the reasoning beside it.
#
#      Worth stating once so the next missing-tool failure is read correctly:
#      `git` is the *only* thing riabuild expects a server to already have.
#      Everything else it needs it downloads and verifies itself, and it
#      unpacks what it downloads in-process, so a future "command not found"
#      on this container means riabuild grew a dependency on the host — not
#      that the image is one `apt-get install` short.
#
# WHERE IT STOPS IS NO LONGER SOMETHING THIS HEADER PREDICTS. It said "the
# binary-install step" for two releases after the install started working, and
# then "the sign-in step" while that was true. Both were paragraphs a reader
# had to trust and nothing had to keep honest. `known_gap` below now decides
# from evidence instead — the stand-in's own log, naming the route it does not
# implement — and the banner it prints is written from the run rather than
# from here.
#
# THE CLIPBOARD CHANNEL IS NOT TESTED HERE. `channel.sh`, beside this file,
# covers it — and runs to the end, because it copies a musl binary onto the
# container instead of installing one. Act two now borrows that trick, so the
# two scripts differ in what they point it at rather than in whether they use
# it: `channel.sh` drives the clipboard transport with it, act two drives the
# provisioning DAG.
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
# Act two's binary. The laptop's build runs here, on the runner; the server's
# runs inside a Debian container whose glibc is older, so it cannot be the same
# file. Same variable, same default and same target as `channel.sh`, so a
# developer who has built one script's server binary has built the other's.
SERVER_BIN="${RIABUILD_SERVER_BIN:-$repo_root/riabuild-cli/target/x86_64-unknown-linux-musl/release/riabuild}"
MEMBER_A="11111111-1111-4111-8111-111111111111"
MEMBER_B="22222222-2222-4222-8222-222222222222"

# NOTHING THIS RUN OWNS IS NAMED THE SAME WAY TWICE.
#
# This script used to hard-code the container name `riabuild-e2e`, the image
# tag `riabuild-e2e`, port 2222 and stub port 8791, and its cleanup ran
# `docker rm -f riabuild-e2e` unconditionally. Two runs at once — two
# worktrees, a developer poking at it while CI runs on the same self-hosted
# box — therefore deleted each other's container mid-assertion, and the loser
# reported a product failure. `channel.sh` documents avoiding exactly this
# collision against *this* script, and this script never got the other half.
#
# The token comes from `mktemp -d`, which the kernel already made unique for
# this process, so nothing new has to be seeded or guarded.
token="$(basename "$work")"
CONTAINER="riabuild-e2e-$token"
IMAGE="riabuild-e2e:$token"

# Ports asked of the kernel rather than picked. A fixed 2222 is somebody's
# existing tunnel as often as it is free, and a fixed stub port is the same
# collision one layer up: two runs would each bind one and the second would
# fail at a place that reads like the stub being broken.
#
# The bind-and-release window is a race in principle. In practice the port is
# taken again microseconds later by this run, and the alternative — a fixed
# number — is not a smaller race but a guaranteed collision.
free_port() {
  python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()'
}
# Assigned after the tool checks below, not here: `free_port` needs python3,
# and a missing one has to surface as the sentence that names what python3 is
# for rather than as a syntax error out of a `$(...)`.
STUB_PORT="${STUB_PORT:-}"
CONTAINER_PORT="${CONTAINER_PORT:-}"

stub_pid=""
agent_pid=""
api_tunnel_pid=""
cleanup() {
  [ -n "$stub_pid" ] && kill "$stub_pid" >/dev/null 2>&1 || true
  [ -n "$agent_pid" ] && kill "$agent_pid" >/dev/null 2>&1 || true
  [ -n "$api_tunnel_pid" ] && kill "$api_tunnel_pid" >/dev/null 2>&1 || true
  # By this run's own name, never by a name a second run also answers to.
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  # And the image with it: a tag per run would otherwise leave one dangling
  # layer set per invocation on whatever machine this is.
  docker rmi -f "$IMAGE" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

echo "== riabuild remote e2e =="

if [ ! -x "$RIABUILD_BIN" ]; then
  echo "RIABUILD_BIN ($RIABUILD_BIN) is not an executable. Build it first:" >&2
  echo "  cd riabuild-cli && cargo build --release --locked" >&2
  exit 1
fi

# Checked here, beside the laptop's binary and before a container is built, for
# the reason prerequisite 3 gives: act two is four minutes into the run, and a
# missing file discovered there costs the whole of act one to find out.
if [ ! -x "$SERVER_BIN" ]; then
  echo "RIABUILD_SERVER_BIN ($SERVER_BIN) is not an executable." >&2
  echo "Act two runs this branch's riabuild on the server, and the server is a" >&2
  echo "Debian container — so it needs a static musl build:" >&2
  echo "  rustup target add x86_64-unknown-linux-musl" >&2
  echo "  cd riabuild-cli && CC_x86_64_unknown_linux_musl=musl-gcc \\" >&2
  echo "    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \\" >&2
  echo "    cargo build --release --locked --target x86_64-unknown-linux-musl" >&2
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

[ -n "$STUB_PORT" ] || STUB_PORT="$(free_port)"
[ -n "$CONTAINER_PORT" ] || CONTAINER_PORT="$(free_port)"

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

# A build context of this run's own, never the source tree. `cp … "$here/
# authorized_keys"` wrote a key into the checkout and deleted it in the trap,
# so an interrupted run left one behind and two concurrent runs overwrote each
# other's — the loser building an image trusting a key it does not hold.
# `channel.sh` already builds this way and says so; this is the other half.
ctx="$work/context"
mkdir -p "$ctx"
cp "$here/Dockerfile" "$ctx/Dockerfile"
cp "$work/seed.pub" "$ctx/authorized_keys"

eval "$(ssh-agent -s)" >/dev/null
agent_pid="$SSH_AGENT_PID"
ssh-add "$work/seed" >/dev/null 2>&1

echo "-- building the container"
docker build -q -t "$IMAGE" "$ctx" >/dev/null
docker run -d --name "$CONTAINER" -p "$CONTAINER_PORT:22" "$IMAGE" >/dev/null

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
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi

# `--accept-host-key` holds the run to this exact fingerprint: a mismatch
# fails host_key::trust_host outright, rather than being pinned on sight the
# way an unadorned run would.
ssh-keyscan -p "$CONTAINER_PORT" -t ed25519 localhost 2>/dev/null > "$work/known_hosts"
fingerprint="$(ssh-keygen -lf "$work/known_hosts" 2>/dev/null | awk '{print $2}')"
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
# the stub was actually asked for. *Which route* the stand-in had no answer
# for is the whole difference between "this harness is incomplete", which is
# tracked, and "riabuild started calling something unexpected", which is a bug
# — and from riabuild's side the two are one HTTP status.
# A plain redirect, deliberately not `| tee`: in a pipeline `$!` is the *last*
# element, so `stub_pid` would name `tee` and the teardown below would leave
# python3 holding the port. The log is printed on any failure that is not a
# tracked gap, which is the only time its contents matter.
python3 "$here/stub_web.py" "$STUB_PORT" "$work/members.json" \
  >"$work/stub_web.log" 2>&1 &
stub_pid=$!
stub_ready=""
for _ in $(seq 1 40); do
  if curl -sf "http://127.0.0.1:$STUB_PORT/api/v1/org/config" >/dev/null 2>&1; then
    stub_ready=1
    break
  fi
  sleep 0.25
done
if [ -z "$stub_ready" ]; then
  echo "stub_web.py never answered on $STUB_PORT" >&2
  cat "$work/stub_web.log" >&2 || true
  exit 1
fi

# ---------------------------------------------------------------------------
# The stand-in riabuild-web, reachable from inside the container
# ---------------------------------------------------------------------------
#
# Item (d) in the header. The laptop is told which dashboard to use with
# `RIABUILD_API_URL` below; the *server* is not, because `env_prefix` carries
# four variables and that is not one of them. Two pieces close the gap, and
# both belong to the container rather than to riabuild:
#
#   * a plain TCP reverse forward, so `127.0.0.1:$STUB_PORT` means the same
#     thing on both sides of the connection. TCP, not the unix-domain
#     `streamlocal-forward@openssh.com` the clipboard channel used to use and
#     the 2026-08-13 exec-transport design removed — this is the harness
#     wiring its own container up, not riabuild asking a server for a
#     permission. `ExitOnForwardFailure` so a port already taken inside the
#     container is an ssh that exits rather than one that looks connected and
#     forwards nothing;
#   * `~/.ssh/environment`, which sshd applies to every session on this
#     account — including the non-interactive `env … riabuild` remote mode
#     runs. `env` adds to the environment it inherits rather than replacing
#     it, so riabuild's own four variables and these two all arrive.
#
# Written at 0600 and owned by `shared`: sshd refuses to read the file
# otherwise, and refuses silently.
echo "-- pointing the container at the stand-in riabuild-web"
docker exec -u shared "$CONTAINER" sh -c \
  "umask 077 && printf 'RIABUILD_API_URL=http://127.0.0.1:%s\nRIABUILD_WEB_URL=http://127.0.0.1:%s\n' \
     '$STUB_PORT' '$STUB_PORT' > ~/.ssh/environment"

ssh -N -R "$STUB_PORT:127.0.0.1:$STUB_PORT" \
  -F /dev/null -p "$CONTAINER_PORT" -i "$work/seed" \
  -o UserKnownHostsFile="$work/known_hosts" \
  -o StrictHostKeyChecking=yes \
  -o IdentitiesOnly=yes \
  -o ExitOnForwardFailure=yes \
  shared@localhost >"$work/api-tunnel.log" 2>&1 &
api_tunnel_pid=$!

# Proven, not assumed, and before anything depends on it: an unreachable
# stand-in otherwise surfaces four steps later as riabuild failing to sign the
# server in, which reads exactly like the product bug it is not.
#
# `bash`'s `/dev/tcp`, because the image has neither `curl` nor `wget` and
# adding one to it would be adding a tool to the machine this test is meant to
# find riabuild unprovisioned. A status line is asked for and read: sshd
# creates the listener before the forward is usable, so "the port accepts a
# connection" is not the same fact as "the stand-in answered".
api_reachable=""
for _ in $(seq 1 40); do
  if docker exec -u shared "$CONTAINER" bash -c \
    "exec 3<>/dev/tcp/127.0.0.1/$STUB_PORT \
       && printf 'GET /api/v1/org/config HTTP/1.0\r\n\r\n' >&3 \
       && head -1 <&3" 2>/dev/null | grep -q '200 OK'; then
    api_reachable=1
    break
  fi
  sleep 0.25
done
if [ -z "$api_reachable" ]; then
  echo "the stand-in riabuild-web is not reachable from inside the container." >&2
  echo "Without it the server's own riabuild talks to the real deployment with" >&2
  echo "a token this stand-in minted, and every failure after this point is a" >&2
  echo "harness problem wearing riabuild's error messages." >&2
  cat "$work/api-tunnel.log" >&2 || true
  exit 1
fi
echo "-- the container reaches the stand-in on 127.0.0.1:$STUB_PORT"

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

in_container() { docker exec "$CONTAINER" su - shared -c "$1"; }

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

  # Branch 3 is the one that changed shape, and the shape is the point.
  #
  # It used to be `replied with HTTP 501` in riabuild's log plus
  # `POST /api/v1/cli/sessions.* 501` in the stub's — matching the stock 501
  # `BaseHTTPRequestHandler` gives an *unimplemented method*. `stub_web.py`
  # had no `do_POST`, so `session::ensure`'s very first request matched, this
  # function returned 0, and the script exited before every assertion below
  # it. It reported a pass, for years, having tested none of what it names.
  # The stub implements `do_POST` now and no stock 501 is reachable, so that
  # pattern is gone rather than kept as a comment about something that cannot
  # happen.
  #
  # What replaces it forgives a *fact this run recorded*, never a sentence
  # riabuild happened to print. `stub_web.py` writes one line —
  # `UNIMPLEMENTED <method> <path>` — before it answers a route it does not
  # have, so "this harness is incomplete" is a statement the harness makes
  # about itself. Anything else is riabuild's failure and is fatal below.
  #
  # Three guards on it, because "the log has a line in it somewhere" would
  # forgive a genuine failure that happened to follow a harmless unimplemented
  # call:
  #
  #   * the stand-in has to have refused a route, and the path has to look like
  #     `/api/v1/…`, so a bogus route cannot be swallowed as a stand-in gap;
  #   * that refusal has to be *what riabuild is complaining about*. The
  #     stand-in answers with an error envelope, so its own sentence — "stub_web
  #     has no route for …" — is printed verbatim by the CLI. Requiring it here
  #     is what ties the forgiveness to the failure rather than to the log;
  #   * and every unimplemented route is named in the banner, so a route nobody
  #     expected is *read* rather than counted.
  #
  # If riabuild ever stops relaying that sentence, this branch stops matching
  # and the run fails loudly. That is the safe direction to be wrong in, and
  # the opposite of the direction the old 501 branch was wrong in.
  if grep -qE '^stub_web: UNIMPLEMENTED [A-Z]+ /api/v1/' "$work/stub_web.log" 2>/dev/null \
    && grep -q "stub_web has no route for" "$work/ada.log"; then
    return 0
  fi

  # There is no branch 4. It used to forgive item (a) — a runner with no
  # libsecret, where `for_account` errored instead of falling back, stopping
  # CI one stage *earlier* than a developer machine with a keyring. That was
  # a real gap in riabuild rather than in this harness, and it is fixed: a
  # machine whose keyring does not answer now keeps a server's session in a
  # 0600 file. So CI and a laptop stop at the same place, and this job stops
  # being the one that proves least.
  #
  # Do not restore it. A keyring failure reaching this script again is a
  # regression in `keychain::keyring_answers`, and forgiving it here is how
  # it would ship unnoticed.
  return 1
}

# Every route the stand-in was asked for and had no answer to, deduplicated
# and one per line. Printed in the banner: a gap that is named is a gap
# somebody can close, and it is the only thing standing between this run and
# the block of assertions at the bottom.
unimplemented_routes() {
  sed -n 's/^stub_web: UNIMPLEMENTED /  /p' "$work/stub_web.log" 2>/dev/null \
    | sort -u
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

# Where `install::ensure_riabuild` puts a server's riabuild:
# `RealPaths::with_root(home, home).riabuild_dir(version)` is
# `tools_root()/riabuild/<version>`, and `tools_root()` is `$HOME/.riabuild`
# whatever `RIABUILD_ROOT` says — toolchains are shared, state is per-developer.
# Spelled out rather than globbed: the version is the one this script resolved
# and handed the stand-in, so a binary appearing under some *other* version is a
# disagreement worth failing on rather than one to glob past.
published_binary="/home/shared/.riabuild/riabuild/$version/riabuild"

# The boundary act one stops judging at, and the whole reason it can stop there
# honestly.
#
# `ensure_riabuild` fetches the checksums file, refuses a platform with no
# digest in it, downloads the tarball, refuses bytes that do not hash to that
# digest, extracts, streams the binary over SSH and re-reads its digest on the
# far side. None of that is faked here and none of it is skipped: this asserts
# the *outcome* of all of it, against the container, in the two ways a black box
# has — the file is at the path riabuild computes, and the thing at that path is
# the release, which it proves by running it and reading back the version this
# script asked for.
#
# `--version` and not `test -x`: an executable bit says a file was chmod'd, and
# what this is here to catch is a binary that landed truncated, or built for the
# wrong platform, or is a *previous* version left over from an install that
# silently did nothing.
assert_installed_the_published_release() {
  local reported
  reported="$(in_container "'$published_binary' --version 2>/dev/null" || true)"
  case "$reported" in
    *"$version"*)
      echo "-- the published riabuild v$version installed, and runs on the server"
      ;;
    *)
      echo "riabuild remote did not install the published release." >&2
      echo "  expected $published_binary to report $version" >&2
      echo "  it said: '$reported'" >&2
      echo >&2
      echo "That is the download-and-verify path failing, which is this" >&2
      echo "branch's code and is act one's to judge. Nothing below runs." >&2
      exit 1
      ;;
  esac
}

# The server's own riabuild writes one line per run to
# `<namespace>/logs/riabuild.log` — `provision::report::log_run`, after the
# tasks, whatever the tasks did. Its presence is the fact that separates "the
# laptop handed off and the DAG ran" from "the run died before the server ever
# got going", and both acts read it: act one to decide whether a failure is
# still its own, act two to assert on what its own run recorded.
namespace="/home/shared/.riabuild-remote/$MEMBER_A"
server_run_log() {                # server_run_log — the last line, or nothing
  in_container "tail -n 1 '$namespace/logs/riabuild.log' 2>/dev/null" || true
}

# One field out of that line, by name. The fields are printed in a fixed order,
# but reading them positionally is how a field added in front of them turns
# every assertion here into a silent pass.
log_field() {                     # log_field <line> <field-name>
  local rest="$1"
  case "$rest" in
    *"$2=["*) ;;
    *) return 1 ;;
  esac
  rest="${rest#*"$2"=[}"
  printf '%s' "${rest%%]*}"
}

# Act one's assertions, run on every path rather than only on the forgiving
# one. They used to sit inside the `known_gap` branch, which meant a run that
# ended some *other* way was never asked whether it had reached the server at
# all — and act two below is about to spend real time on this container, so
# "did act one actually happen" has to be answered before it does.
assert_reached_the_server
assert_installed_the_published_release

# How act one ended, decided once and acted on after act two. Three outcomes,
# and none of them is "carry on regardless":
#
#   gap      — a route `stub_web.py` does not implement, on the evidence of the
#              stand-in's own log plus riabuild relaying its sentence. Exactly
#              as before; `known_gap` is unchanged.
#   deferred — the laptop handed off and the *server's* riabuild ran the DAG
#              and recorded it. That server is the published release, so the
#              outcome is last month's code and act one is not the thing that
#              can judge it. Handed to act two, which runs the same DAG with
#              this branch's binary and does judge it. A deferral to a named,
#              executing assertion, not an exemption: act two failing fails
#              the job.
#   neither  — fatal, exactly as before. A failure with no server-side run log
#              behind it is one the laptop owns, and the laptop is this branch.
act_one_gap=""
act_one_deferred=""
act_one_line=""
if [ "$ada_status" -ne 0 ]; then
  if known_gap; then
    act_one_gap=1
  else
    act_one_line="$(server_run_log)"
    if log_field "$act_one_line" failed >/dev/null 2>&1; then
      act_one_deferred=1
      echo
      echo "-- act one stopped past the install, inside the released server's"
      echo "   task DAG. Its run log: $act_one_line"
      echo "   That is v$version's code, not this checkout's. Act two runs the"
      echo "   same DAG with this branch's binary and asserts the outcome."
    else
      echo "riabuild remote failed for a reason that is not the known gap," >&2
      echo "and the server's own riabuild recorded no run — so this is not" >&2
      echo "something act two can be asked about:" >&2
      # The stub's own log, which is no longer on stdout. Without it an HTTP
      # failure here shows riabuild's side of the exchange and not what the
      # stub was asked for, and those two together are what say whether this
      # is a harness limitation or a real regression.
      echo "--- stub_web ---" >&2
      tail -20 "$work/stub_web.log" >&2 2>/dev/null || true
      exit 1
    fi
  fi
fi

# ---------------------------------------------------------------------------
# ACT TWO — this branch's riabuild, on the server
# ---------------------------------------------------------------------------
#
# See the header. Everything above ran the released binary on the far side and
# could not have done otherwise; everything below runs the binary built from
# this checkout, and is therefore the half of this job that gates the change.

echo
echo "== act two: this branch's riabuild, running the server side =="

# The same option list `remote::identity::ssh_options` stands for, and the same
# one the API tunnel above was opened with. Deliberately the seed key rather
# than riabuild's own: act one has already proven riabuild authorised its key,
# and reusing it here would make act two's first failure a second reading of
# act one's last assertion.
server_ssh=(
  -F /dev/null
  -p "$CONTAINER_PORT"
  -i "$work/seed"
  -o UserKnownHostsFile="$work/known_hosts"
  -o StrictHostKeyChecking=yes
  -o IdentitiesOnly=yes
  -o BatchMode=yes
)

# Over SSH rather than `docker exec`, which is not fussiness: `~/.ssh/environment`
# — the file that tells the server's riabuild where riabuild-web is — is applied
# by *sshd*, to sshd's own sessions. A `docker exec` inherits none of it and
# would talk to the real deployment holding a token this stand-in minted.
# `timeout` for the reason `run_as` has one: a silent wait is the single failure
# a CI log cannot diagnose.
on_server() {                     # on_server <shell command>
  timeout 600 ssh "${server_ssh[@]}" shared@localhost "$1"
}

# Act two runs under the session act one minted, not under a `RIABUILD_TOKEN`
# of its own. `session::ensure` wrote it to `<namespace>/session.token` — a
# server keeps its session in a file, never in the keychain — and reusing it is
# what makes act two's requests authenticate the way a real server's do:
# delegated, one hop, checked for expiry and revocation on every call.
#
# Checked, not assumed, and fatal when it is missing. `RIABUILD_TOKEN` would
# win over the file if it were set (`keychain::selection::select` puts it
# first), and setting it here would quietly paper over exactly the case this
# guard is for: act one not reaching the sign-in at all. Act two is the half of
# this job that gates the change, so a run where it *cannot* execute has to be
# red — a gate that skips itself is the pathology this whole file is about.
# Today it cannot happen: item (b) is closed and act one signs the server in.
if ! in_container "test -s '$namespace/session.token'" >/dev/null 2>&1; then
  echo "Act one minted no server session — there is no $namespace/session.token." >&2
  echo "Act two has nothing to authenticate as, so the half of this job that" >&2
  echo "tests this branch's server code cannot run. That is a regression in" >&2
  echo "the sign-in path (header item (b), closed), not a harness gap." >&2
  exit 1
fi

# `/home/shared/riabuild`, never over the versioned path act one installed to.
# `published_binary` is the evidence for act one's assertion, and overwriting it
# would erase what this script has just proved — and a later `riabuild remote`
# against this container would find a digest that is not the release's and
# reinstall, which reads as flakiness rather than as this script having moved
# the file.
branch_binary="/home/shared/riabuild"
echo "-- copying this branch's musl build onto the container"
docker cp "$SERVER_BIN" "$CONTAINER:$branch_binary" >/dev/null
docker exec "$CONTAINER" chown shared:shared "$branch_binary"
docker exec "$CONTAINER" chmod 755 "$branch_binary"

# The prefix `remote::env_prefix` builds, composed here because act two has no
# `riabuild remote` to build it. Three of its four variables: `RIABUILD_ROOT`
# and `RIABUILD_REMOTE` are set *together* or `scope::detect` refuses the run
# outright — deliberately, because one without the other is how a bearer token
# once reached a laptop's shared path — and `CLOUDCLI_NO_TMUX` rides along
# exactly as it does on the wire.
#
# `RIABUILD_CHANNEL_SOCKET` is the one left out, and left out rather than
# forgotten: nothing here serves a pump, `--no-shell` starts none, and naming a
# socket nobody is listening on would have the shims fail for a reason act two
# has nothing to say about. `channel.sh` is where that variable is under test.
#
# The name is `localhost`, which is what act one's run allocated: `ask_name`
# defaults to the first label of the hostname and an unattended run takes the
# default. Only its non-emptiness is load-bearing for `scope::detect`, but using
# the same one keeps act two inside act one's namespace rather than beside it.
branch_prefix="env 'RIABUILD_ROOT=$namespace' 'RIABUILD_REMOTE=localhost' 'CLOUDCLI_NO_TMUX=1' '$branch_binary'"

# The two hops `flow::connect` makes before the setup run, in its order and for
# its reasons. Sweeping first can only ever clear a session that already ended;
# sweeping after seeding would let this run's own exit wipe the credential it
# had just written. Both are riabuild's own `internal` subcommands — act two
# composes the call, never the behaviour.
#
# Non-fatal, both, because `seed_github` itself is: a laptop with no sign-in to
# lend prints a note and carries on. What a failed seed costs shows up in the
# run log's `failed=` field, which is printed either way.
echo "-- sweeping, and lending this laptop's GitHub sign-in"
on_server "$branch_prefix internal gh-sweep" >"$work/act2-sweep.log" 2>&1 || true
# On stdin, never in argv, for the reason `seed.rs` gives: `ps` is readable by
# every other developer sharing this account.
printf '%s' "$token" \
  | on_server "$branch_prefix internal seed-github" >"$work/act2-seed.log" 2>&1 || true

echo "-- running the setup flow on the server"
set +e
on_server "$branch_prefix --no-shell --quiet" >"$work/act2.log" 2>&1
act2_status=$?
set -e
cat "$work/act2.log"

if [ "$act2_status" -eq 124 ]; then
  echo "the server-side setup run hung and was killed at 600s. A hang is a" >&2
  echo "failure: something is waiting on input that cannot arrive." >&2
  exit 1
fi

# What act two asserts, and why it is these three.
#
# The exit status is NOT one of them, and that is act one's boundary drawn
# again rather than a second helping of forgiveness. Past the toolchain the DAG
# wants an Infisical the container cannot reach — item (e) in the header, still
# open, and a piece of work rather than a fix. So act two asserts what it can
# observe, names what it could not reach, and claims nothing about it.
act2_failures=""
note_failure() { act2_failures="$act2_failures\n  - $1"; }

act2_line="$(server_run_log)"
act2_failed=""
act2_skipped=""
if ! log_field "$act2_line" failed >/dev/null 2>&1; then
  note_failure "the server wrote no usable run log (last line: '$act2_line')"
else
  # (1) The run log has to have been written by *this* build. A local `cargo
  # build` gets `9999.0.0-dev` from `version::VERSION` — the sentinel for "no
  # release tag injected one" — and the release act one installed reports a
  # date. Without this, everything below could be satisfied by act one's
  # leftovers on a server act two never actually ran on.
  case "$act2_line" in
    *"riabuild 9999.0.0-dev "*) ;;
    *) note_failure "the last run log line is not from a local build: '$act2_line'" ;;
  esac

  act2_failed="$(log_field "$act2_line" failed)"
  act2_skipped="$(log_field "$act2_line" skipped)"

  # Failed and skipped are told apart rather than lumped together, because they
  # say opposite things about this branch. `toolchain` in `failed` is the bug
  # this act exists for; `toolchain` in `skipped` means something upstream of it
  # went down and act two never reached the assertion at all.
  toolchain_state=ran
  case ",$act2_failed," in
    *,toolchain,*) toolchain_state=failed ;;
    *)
      case ",$act2_skipped," in
        *,toolchain,*) toolchain_state=skipped ;;
      esac
      ;;
  esac

  case "$toolchain_state" in
    failed)
      # (2) riabuild's own verdict on the task this branch changed. This is the
      # "it did not take effect" hard error — apply ran and the re-check
      # disagreed — which is exactly what v2026.08.21.1 does on this container
      # and exactly what the change is for.
      note_failure "riabuild recorded the toolchain task as failed"
      ;;
    skipped)
      # Still red, because a gate that could not execute is not a pass — but
      # named for what it is, so nobody reads it as a pnpm bug.
      note_failure "the toolchain task was skipped behind failed=[$act2_failed]"
      ;;
    *)
      # (3) The property behind (2), asserted from outside riabuild so that a
      # task reporting satisfied on a broken machine cannot pass it. pnpm's own
      # `-v`, through the shim `toolchain::ensure_pnpm` writes, on a container
      # that has no `libatomic.so.1` and never will: the released build puts
      # pnpm's platform executable at this path, and it exits 127.
      pnpm_reported="$(in_container "'$namespace/bin/pnpm' -v 2>/dev/null" || true)"
      case "$pnpm_reported" in
        [0-9]*) echo "-- pnpm answers $pnpm_reported on the server" ;;
        *)
          note_failure \
            "pnpm does not run on the server ($namespace/bin/pnpm -v said '$pnpm_reported')"
          ;;
      esac
      ;;
  esac
fi

if [ -n "$act2_failures" ]; then
  echo >&2
  echo "Act two failed. This is the half of the job that runs THIS branch on" >&2
  echo "the server, so these are regressions rather than harness gaps:" >&2
  printf "%b\n" "$act2_failures" >&2
  echo >&2
  echo "--- the server's run log ---" >&2
  echo "$act2_line" >&2
  exit 1
fi

echo
echo "-- act two passed: $act2_line"
# Named rather than implied. `failed` and `skipped` here are the part of the DAG
# this container cannot finish, and printing them is the bargain
# `unimplemented_routes` makes — a gap that is named is a gap somebody can
# close, and one that is merely tolerated is one nobody will.
echo "   still out of this container's reach —"
echo "     failed:  $(log_field "$act2_line" failed)"
echo "     skipped: $(log_field "$act2_line" skipped)"
echo "   see item (e) in this file's header."

if [ -n "$act_one_gap" ]; then
  echo
  # Which gap, in the run's own words rather than a fixed paragraph. Two
  # earlier versions of this banner named a cause from a constant here: one
  # went on announcing a missing musl checksum for two releases after that
  # shipped, and the next announced a stub with no `do_POST` while the run was
  # in fact stopping somewhere else. Nothing below is written in advance —
  # `stub_web.py` says which routes it has no answer for, and this prints
  # them.
  echo "############################################################"
  echo "# KNOWN GAP, not a regression: the stand-in riabuild-web is"
  echo "# missing routes this run needed."
  echo "#"
  echo "# stub_web.py was asked for, and has no answer to:"
  unimplemented_routes | sed 's/^/# /'
  echo "#"
  echo "# That is a limitation of this harness, not of riabuild — see"
  echo "# item (e) in this file's header. Each route added there gets"
  echo "# the run one step further; the block of assertions below runs"
  echo "# when none are left."
  echo "#"
  echo "# Asserted just now, not assumed: a key pair was generated,"
  echo "# the container's host key was pinned, riabuild's key was"
  echo "# authorised on the container, and the published v$version was"
  echo "# downloaded, verified, installed and run there. Those stages"
  echo "# ran for real."
  echo "#"
  echo "# The two-developer assertions this test names were NOT run."
  echo "# They need a fully provisioned server, and act one is what"
  echo "# would have to reach one. Nothing here has yet proved the"
  echo "# namespace isolation remote mode rests on."
  echo "#"
  echo "# Act two DID run, and passed: this branch's riabuild drove"
  echo "# the server's own task DAG. What act one lost above is the"
  echo "# laptop half of the handoff, not the server half."
  echo "#"
  echo "# The clipboard channel is NOT among what went untested here:"
  echo "# channel.sh covers it against this same container and runs to"
  echo "# the end. Remote mode's channel wiring still is."
  echo "############################################################"
  exit 0
fi

if [ -n "$act_one_deferred" ]; then
  echo
  echo "############################################################"
  echo "# Act one stopped inside the RELEASED server's task DAG, and"
  echo "# act two passed."
  echo "#"
  echo "# What act one proved, asserted just now and not assumed: a"
  echo "# key pair was generated, the container's host key was pinned,"
  echo "# riabuild's key was authorised, and riabuild downloaded,"
  echo "# verified and installed the published v$version — which runs"
  echo "# on the server."
  echo "#"
  echo "# What it then stopped on belongs to v$version and cannot be"
  echo "# changed from this checkout:"
  echo "#   $act_one_line"
  echo "#"
  echo "# That same DAG, driven by THIS branch's binary, is what act"
  echo "# two just asserted. A server-side regression this branch"
  echo "# introduces makes act two red; it does not reach this banner."
  echo "#"
  echo "# The two-developer assertions still did NOT run: they need a"
  echo "# riabuild remote that reaches the end, which needs the routes"
  echo "# and the Infisical stand-in item (e) names."
  echo "############################################################"
  exit 0
fi

# EVERYTHING BELOW THIS LINE IS GATED, AND HAS NEVER RUN.
#
# Not gated by a flag and not skipped: one of the two `exit 0`s above is
# reached first, every time, because act one stops short of a provisioned
# server. What stops it is `stub_web.py` — three GET routes and one POST
# against a server that runs riabuild's whole task DAG — plus the Infisical
# stand-in the container cannot reach, and the banner above names whichever of
# those the last run actually wanted. Item (e) in this file's header is the
# same fact stated once, up there, for a reader who starts at the top.
#
# ACT TWO IS NOT A SUBSTITUTE FOR THIS BLOCK, and reading it as one is the
# mistake this paragraph exists to prevent. Act two drives the server's own
# riabuild for a single developer, with the prefix composed by hand; what is
# written below is *two* developers, each arriving through a whole
# `riabuild remote`, and the isolation between them. The half act two cannot
# reach is exactly the half these lines are about.
#
# It is written down here rather than in the header alone because this is
# where somebody stands when they wonder why an assertion they can read is not
# protecting them. Until act one reaches the end, none of these lines has ever
# executed — including the two below that were added specifically to catch a
# bug that had already shipped.
echo "-- ada's run reached the end. Running the real assertions."

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
# status line: one script, at the path the settings above actually name
#
# The counterpart to those two assertions, and the case where the namespace is
# the *wrong* answer. Those settings name `node ~/.riabuild/claude-statusline.js`;
# Claude Code runs it through a shell, so `~` is this box's shared account. The
# task built that path on `RIABUILD_ROOT` until 2026-08-17 and therefore wrote
# into each namespace instead — `node` was handed a path that does not exist, and
# a status line whose command fails renders as no status line at all. Nothing
# errored, nothing was logged, and the task reported satisfied on both runs.
#
# Written here, and — like every assertion in this block — NOT RUNNING YET, for
# reason (2) in the header. It is recorded rather than claimed: the live gate on
# that bug is `claude_statusline`'s own unit tests, which now build a server
# shape via `testing::ctx_on_a_server` instead of the laptop every test here had
# been built on. This line is what starts covering it from outside the binary the
# moment the run gets past the install step.
in_container "test -s ~/.riabuild/claude-statusline.js"
# and nowhere else — a copy in a namespace is byte-identical to the live script,
# so it answers "is the script installed?" with a yes that means nothing.
in_container "! test -e ~/.riabuild-remote/$MEMBER_A/claude-statusline.js"
in_container "! test -e ~/.riabuild-remote/$MEMBER_B/claude-statusline.js"

# Names what was checked rather than counting it, and never claims "all
# assertions passed" — that phrasing is how this script once reported a run
# that never reached the server, and the count is what used to make every
# branch that changed the set conflict with every other one.
echo "remote mode e2e passed — separate namespaces, separate git identities,"
echo "one shared toolchain, per-developer checkouts, no gh credential left"
echo "behind, the team's Claude Code settings in each namespace, and the status"
echo "line script at the one path those settings name."
