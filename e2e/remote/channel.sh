#!/usr/bin/env bash
# The clipboard channel, end to end: a real laptop clipboard, a real reverse
# SSH forward, and a real shim on a real server.
#
# Two properties, and the second one is why the feature is defensible at all.
#
# 1. THE CHANNEL CARRIES THE CLIPBOARD. A PNG and a UTF-8 string put on the
#    laptop's clipboard are pasted by the shim on the server, byte for byte,
#    and a copy made on the server lands back on the laptop's clipboard.
#
# 2. ITS ABSENCE DEGRADES TO "NO CLIPBOARD", NEVER TO "ENVIRONMENT BROKEN".
#    `channel/mod.rs`'s module doc promises that a laptop that closes its lid
#    leaves a session that still runs setup, still re-pulls rotated secrets and
#    still opens a shell, and that only paste stops. This script kills the
#    tunnel mid-session and holds the whole promise to account: setup re-runs
#    and reaches riabuild-web, the environment shell still opens with riabuild's
#    PATH and `BROWSER`, and only the clipboard fails — a read degrading to an
#    empty clipboard, a write failing loudly rather than losing what a developer
#    copied.
#
# THE HEADLESS CLIPBOARD: WHY XVFB AND XCLIP RATHER THAN A SUBSTITUTE.
# `clipboard::detect` asks the runner for `wl-paste`, then `xclip`, and the
# backend behind either one drives that binary through `CommandRunner`. A
# Linux CI runner has no display, so there are two ways to give the agent a
# clipboard: run a real X server and a real `xclip` under it, or hand the agent
# a stand-in binary named `xclip` that a test controls.
#
# This takes the first. The second would re-test what
# `clipboard/linux.rs`'s unit tests already cover far more thoroughly — a
# scripted `xclip` is exactly what `FakeRunner` is — while quietly dropping the
# only things a real one can prove: that riabuild's argv matches what xclip
# actually accepts, that a PNG survives X11's atom vocabulary in both
# directions, and that `run_forking` really does return rather than blocking on
# the forked selection owner that `xclip -i` leaves behind. Those are the
# failures that only appear against the real tool, which is the entire reason
# to spend an Xvfb on it. The cost is two packages on the runner (`xvfb` and
# `xclip`) and about a second of startup.
#
# TWO STAND-INS, BOTH NAMED RATHER THAN HIDDEN.
#
# a. The server's riabuild is copied in rather than installed. `riabuild remote`
#    installs it by downloading a published release and verifying its digest,
#    and no published release has an `x86_64-unknown-linux-musl` checksum yet —
#    the gap `run.sh` documents at length and stops at. So this script copies in
#    a locally built **musl** binary, which is the same target a real install
#    would fetch. What that skips is the install step, which `run.sh` owns; what
#    it exercises is everything after it, which `run.sh` cannot reach.
#
# b. The shim is invoked as `riabuild channel shim xclip …` rather than through
#    `~/.riabuild/bin/xclip`. That generated file is a one-line
#    `exec riabuild channel shim xclip "$@"` (`shims::write_clipboard_shims`),
#    and nothing writes it yet — the function is `#[allow(dead_code)]`, waiting
#    on remote mode's wiring. So this runs what the wrapper would exec, one
#    `exec` short of the whole path, on the real server binary.
#
# WHAT THIS SCRIPT DOES NOT COVER, so that `run.sh` is not assumed to.
#
#   * Remote mode's own wiring — `src/remote/channel.rs`: the supervisor
#     holding the tunnel up and rebuilding it, `RIABUILD_CHANNEL_SOCKET`
#     arriving in the `env 'K=V' … '/abs/riabuild'` prefix rather than being
#     set by a test, and the banner line naming the channel. All of that exists
#     in the tree; none of it is observed here. The tunnel below is an
#     `ssh -N -R` this script owns, deliberately: a supervisor that rebuilds a
#     killed tunnel is the right behaviour and the wrong thing to test a
#     degradation against. Observing it belongs to `run.sh`, once that can get
#     past the install step.
#   * A real secrets *re-pull*. `--check` re-evaluates the `env_local` task
#     alongside the other ten, and this asserts that it does — but pulling
#     secrets for real needs an installed `infisical` and the Infisical stub,
#     which needs the provisioning run the container job cannot finish yet.
#     Asserted here: the task is still evaluated with the channel dead.
#     Not asserted here: secrets actually arriving.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
work="$(mktemp -d)"

# The laptop side runs on this machine, so it is the ordinary host build the CI
# job already produces. The server side runs inside a Debian container whose
# glibc is older than the runner's, so it cannot be the same file — and musl is
# not a workaround for that, it is what release.yml ships for Linux.
LAPTOP_BIN="${RIABUILD_BIN:-$repo_root/riabuild-cli/target/release/riabuild}"
SERVER_BIN="${RIABUILD_SERVER_BIN:-$repo_root/riabuild-cli/target/x86_64-unknown-linux-musl/release/riabuild}"

# Distinct from `run.sh`'s container, image, port and stub port throughout: the
# two scripts run in the same CI job, and a shared name means one cleaning up
# after the other.
CONTAINER="riabuild-e2e-channel"
IMAGE="riabuild-e2e-channel"
PORT="${CHANNEL_PORT:-2223}"
STUB_PORT="${CHANNEL_STUB_PORT:-8792}"
MEMBER="33333333-3333-4333-8333-333333333333"

# The namespace remote mode gives a developer on a shared server, and the socket
# inside it. Developers share one Unix account there, so they share one uid and
# one $XDG_RUNTIME_DIR — the namespace is what stops one developer's xclip
# reading another's laptop, and it is the path this test forwards onto.
remote_dir="/home/shared/.riabuild-remote/$MEMBER"
remote_sock="$remote_dir/channel.sock"
laptop_sock="$work/channel.sock"

xvfb_pid=""
agent_pid=""
tunnel_pid=""
api_pid=""
stub_pid=""
cleanup() {
  # Xvfb first: every xclip holding a selection is a client of it, and killing
  # the server reaps them all. Matching them by name instead would be a `pkill
  # -f xclip` on a machine that may be somebody's desktop.
  # Unset and already-dead are both fine here: `kill` complains and `|| true`
  # swallows it. A guard would only make the list five times as long, and the
  # trap runs on every exit path including the ones that failed early.
  for pid in "$xvfb_pid" "$agent_pid" "$tunnel_pid" "$api_pid" "$stub_pid"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

failures=0
pass() { echo "  ok   $*"; }
fail() { echo "  FAIL $*" >&2; failures=$((failures + 1)); }

echo "== riabuild clipboard channel e2e =="

# Checked up front, all of them, because each one otherwise surfaces in the
# middle of a step as something that reads like a product bug: a missing
# `xclip` as an agent that refuses to start, a missing `Xvfb` as an agent that
# starts and then cannot read anything.
need() {                          # need <command> <why>
  command -v "$1" >/dev/null 2>&1 || {
    echo "$1 is not installed (or not on PATH). $2" >&2
    exit 1
  }
}
need docker "This test runs a real sshd in a container; there is no way to fake that half."
need ssh "The channel is carried by a real reverse SSH forward, which is the thing under test."
need ssh-keygen "This script generates the key the container will trust."
need ssh-keyscan "The container's host key is read with it, so the SSH above can be strict."
need Xvfb "The laptop side needs a real X display for a real xclip to talk to — see the header."
need xclip "The agent drives it to read and write this laptop's clipboard, and detect refuses without it."
need python3 "It runs stub_web.py, so the setup flow on the server has a riabuild-web to reach."

if [ ! -x "$LAPTOP_BIN" ]; then
  echo "RIABUILD_BIN ($LAPTOP_BIN) is not an executable. Build it first:" >&2
  echo "  cd riabuild-cli && cargo build --release --locked" >&2
  exit 1
fi
if [ ! -x "$SERVER_BIN" ]; then
  echo "RIABUILD_SERVER_BIN ($SERVER_BIN) is not an executable." >&2
  echo "The server is a Debian container, so it needs a static musl build —" >&2
  echo "the same target release.yml ships for Linux. Build it with:" >&2
  echo "  rustup target add x86_64-unknown-linux-musl" >&2
  echo "  cd riabuild-cli && CC_x86_64_unknown_linux_musl=musl-gcc \\" >&2
  echo "    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \\" >&2
  echo "    cargo build --release --locked --target x86_64-unknown-linux-musl" >&2
  exit 1
fi

# A build context of our own rather than `$here`. `run.sh` writes its
# `authorized_keys` next to the Dockerfile and deletes it afterwards; doing the
# same here would have the two scripts overwriting one file, and the loser would
# build an image trusting a key it does not hold.
ctx="$work/context"
mkdir -p "$ctx"
cp "$here/Dockerfile" "$ctx/Dockerfile"
ssh-keygen -t ed25519 -N "" -f "$work/key" -C "riabuild channel e2e" >/dev/null
cp "$work/key.pub" "$ctx/authorized_keys"

echo "-- building the container"
docker build -q -t "$IMAGE" "$ctx" >/dev/null
docker run -d --name "$CONTAINER" -p "$PORT:22" "$IMAGE" >/dev/null

echo "-- waiting for sshd"
ready=""
for _ in $(seq 1 30); do
  if ssh-keyscan -p "$PORT" -t ed25519 localhost 2>/dev/null | grep -q ssh-ed25519; then
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
ssh-keyscan -p "$PORT" -t ed25519 localhost 2>/dev/null > "$work/known_hosts"

# Strict host-key checking against the key just read, and riabuild's own
# known_hosts rather than the developer's. `-F /dev/null` keeps a personal
# `~/.ssh/config` — a ProxyCommand, an IdentityAgent, a Host * block — out of a
# connection whose whole point is to be the same on every machine.
ssh_opts=(
  -F /dev/null
  -p "$PORT"
  -i "$work/key"
  -o UserKnownHostsFile="$work/known_hosts"
  -o StrictHostKeyChecking=yes
  -o IdentitiesOnly=yes
  -o ExitOnForwardFailure=yes
)

echo "-- installing the server's riabuild"
docker cp "$SERVER_BIN" "$CONTAINER:/home/shared/riabuild" >/dev/null
docker exec "$CONTAINER" chown shared:shared /home/shared/riabuild
docker exec "$CONTAINER" chmod 755 /home/shared/riabuild
docker exec -u shared "$CONTAINER" mkdir -p "$remote_dir"
# sshd will not bind a forward onto a path that already exists, and the failure
# is a one-line warning on a connection that otherwise looks fine.
docker exec -u shared "$CONTAINER" rm -f "$remote_sock"

# The environment every server-side invocation carries. `RIABUILD_CHANNEL_SOCKET`
# is what remote mode will put in its `env 'K=V' … '/abs/riabuild'` prefix; the
# rest is what lets the setup flow reach a riabuild-web without a browser
# sign-in, exactly as `run.sh` and `e2e/run.sh` do it.
server_env=(
  -u shared
  -e HOME=/home/shared
  -e "RIABUILD_CHANNEL_SOCKET=$remote_sock"
  -e RIABUILD_TOKEN=channel-e2e-token
  -e "RIABUILD_API_URL=http://127.0.0.1:$STUB_PORT"
  -e "RIABUILD_WEB_URL=http://127.0.0.1:$STUB_PORT"
  -e SHELL=/bin/bash
)

on_server() {                     # on_server <riabuild args...>
  docker exec "${server_env[@]}" "$CONTAINER" /home/shared/riabuild "$@"
}
on_server_stdin() {               # on_server_stdin <riabuild args...>, content on stdin
  docker exec -i "${server_env[@]}" "$CONTAINER" /home/shared/riabuild "$@"
}

echo "-- starting riabuild-web's stand-in"
cat > "$work/members.json" <<JSON
{
  "channel-e2e-token": {
    "githubLogin": "ada",
    "memberId": "$MEMBER",
    "firstName": "Ada",
    "lastName": "Lovelace",
    "email": "ada@clubria.dev",
    "role": "developer",
    "status": "active"
  },
  "__version__": "0.0.0"
}
JSON
python3 "$here/stub_web.py" "$STUB_PORT" "$work/members.json" >"$work/stub.log" 2>&1 &
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
  cat "$work/stub.log" >&2 || true
  exit 1
fi

# A second SSH, carrying only the stand-in riabuild-web into the container. It
# is deliberately not the same connection as the channel: the degradation test
# kills the channel's tunnel, and sharing one process would take the API down
# with it and prove nothing about the channel at all.
ssh -N -R "$STUB_PORT:127.0.0.1:$STUB_PORT" "${ssh_opts[@]}" shared@localhost \
  >"$work/api-tunnel.log" 2>&1 &
api_pid=$!

echo "-- starting the laptop's display and clipboard"
# `-displayfd` has Xvfb pick a free display and report it, rather than this
# script guessing `:99` and colliding with whatever else on the machine already
# had that idea.
Xvfb -displayfd 8 -screen 0 1280x1024x24 >"$work/xvfb.log" 2>&1 8>"$work/display" &
xvfb_pid=$!
display=""
for _ in $(seq 1 40); do
  display="$(tr -d '[:space:]' < "$work/display" 2>/dev/null || true)"
  [ -n "$display" ] && break
  sleep 0.25
done
if [ -z "$display" ]; then
  echo "Xvfb never reported a display" >&2
  cat "$work/xvfb.log" >&2 || true
  exit 1
fi
export DISPLAY=":$display"
# On a developer's own Wayland desktop this variable is set, and `detect` prefers
# `wl-paste` when it is — which would have the agent reading the real session's
# clipboard rather than the Xvfb this test controls, and the assertions below
# would be about whatever happened to be on the developer's clipboard.
unset WAYLAND_DISPLAY

put_on_clipboard() {              # put_on_clipboard <x11-target> <file>
  # xclip forks a child to serve the selection and the parent returns; the child
  # is a client of the Xvfb above, so cleanup reaps it. Each call takes the
  # selection from the last one, which is why the cases below run in sequence
  # rather than staging both types at once.
  xclip -selection clipboard -t "$1" -i "$2"
  # The selection owner is established asynchronously. Poll for the type to
  # actually be on offer rather than sleeping a guess.
  for _ in $(seq 1 40); do
    if xclip -selection clipboard -t TARGETS -o 2>/dev/null | grep -qx "$1"; then
      return 0
    fi
    sleep 0.25
  done
  echo "the laptop's own xclip never advertised $1 — the display is not working" >&2
  exit 1
}

echo "-- starting the channel agent on the laptop"
# HOME inside the scratch directory: the agent resolves `~/.riabuild/bin` for
# the browser opener, and a test must not write into the developer's own tree.
mkdir -p "$work/laptop"
env -u RIABUILD_ROOT -u WAYLAND_DISPLAY \
  HOME="$work/laptop" \
  DISPLAY="$DISPLAY" \
  "$LAPTOP_BIN" channel agent --socket "$laptop_sock" >"$work/agent.log" 2>&1 &
agent_pid=$!
socket_up=""
for _ in $(seq 1 40); do
  [ -S "$laptop_sock" ] && { socket_up=1; break; }
  sleep 0.25
done
if [ -z "$socket_up" ]; then
  echo "the channel agent never bound $laptop_sock" >&2
  cat "$work/agent.log" >&2 || true
  exit 1
fi

echo "-- forwarding it onto the server"
ssh -N -R "$remote_sock:$laptop_sock" "${ssh_opts[@]}" shared@localhost \
  >"$work/tunnel.log" 2>&1 &
tunnel_pid=$!

# Poll the server's own probe rather than the socket file: sshd creates the node
# before it is connected, so its existence is not the channel being up. This is
# also the first assertion — `channel status` is what a developer runs to find
# out why paste stopped.
channel_up=""
for _ in $(seq 1 40); do
  if on_server channel status >"$work/status-up.log" 2>&1; then
    channel_up=1
    break
  fi
  sleep 0.5
done

echo
echo "-- the channel carries the clipboard"

if [ -n "$channel_up" ] && grep -q "Clipboard channel — connected" "$work/status-up.log"; then
  pass "riabuild channel status on the server reports the channel connected"
else
  fail "the channel never came up on the server"
  cat "$work/status-up.log" >&2 || true
  cat "$work/tunnel.log" >&2 || true
  # Nothing below can mean anything without a channel, and a run that reports
  # eleven failures for one cause hides the cause.
  exit 1
fi

# Non-ASCII on purpose. The bytes cross X11's UTF8_STRING atom, a length-framed
# protocol and a shim that writes them back out; every stage of that is a place
# a re-encode or a truncation would go unnoticed against plain ASCII.
printf 'Clubria — “paste” ✓ 日本語' > "$work/text.src"
put_on_clipboard UTF8_STRING "$work/text.src"

rc=0
on_server channel shim xclip -selection clipboard -t TARGETS -o \
  >"$work/targets.out" 2>"$work/targets.err" || rc=$?
if [ "$rc" -eq 0 ] && grep -qx "UTF8_STRING" "$work/targets.out"; then
  pass "the shim's TARGETS advertises the laptop's text, in xclip's own vocabulary"
else
  fail "TARGETS did not advertise UTF8_STRING (exit $rc): $(cat "$work/targets.out")"
fi

rc=0
on_server channel shim xclip -selection clipboard -t UTF8_STRING -o \
  >"$work/text.out" 2>"$work/text.err" || rc=$?
if [ "$rc" -eq 0 ] && cmp -s "$work/text.src" "$work/text.out"; then
  pass "a UTF-8 string pastes on the server byte for byte"
else
  fail "the text did not survive the channel (exit $rc)"
  cat "$work/text.err" >&2 || true
fi

# Eight pixels square, so it stays under `resize::MAX_LONG_EDGE` and crosses
# untouched — the byte-for-byte comparison below is only meaningful for an image
# the channel had no reason to re-encode.
python3 - "$work/image.src" <<'PY'
import struct, sys, zlib

def chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

width = height = 8
raw = b"".join(
    b"\x00" + bytes(v for x in range(width) for v in ((x * 31) % 256, (y * 29) % 256, (x * y) % 256))
    for y in range(height)
)
png = (
    b"\x89PNG\r\n\x1a\n"
    + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(raw))
    + chunk(b"IEND", b"")
)
with open(sys.argv[1], "wb") as handle:
    handle.write(png)
PY
put_on_clipboard image/png "$work/image.src"

rc=0
on_server channel shim xclip -selection clipboard -t TARGETS -o \
  >"$work/targets-png.out" 2>/dev/null || rc=$?
if [ "$rc" -eq 0 ] && grep -qx "image/png" "$work/targets-png.out"; then
  pass "the shim's TARGETS advertises the laptop's image"
else
  fail "TARGETS did not advertise image/png (exit $rc): $(cat "$work/targets-png.out")"
fi

rc=0
on_server channel shim xclip -selection clipboard -t image/png -o \
  >"$work/image.out" 2>"$work/image.err" || rc=$?
if [ "$rc" -eq 0 ] && cmp -s "$work/image.src" "$work/image.out"; then
  pass "a PNG pastes on the server byte for byte"
else
  fail "the PNG did not survive the channel (exit $rc; $(wc -c <"$work/image.src") bytes in, $(wc -c <"$work/image.out" 2>/dev/null || echo 0) out)"
  cat "$work/image.err" >&2 || true
fi

# The other direction, which no amount of paste testing reaches: `gh`, `git`,
# `pass` and every `| xclip` script on the server copy into a clipboard the
# developer would otherwise never be able to paste from.
copied='copied on the server ✓ 日本語'
rc=0
printf '%s' "$copied" | on_server_stdin channel shim xclip -selection clipboard -i \
  >"$work/copy.out" 2>"$work/copy.err" || rc=$?
if [ "$rc" -ne 0 ]; then
  fail "a copy on the server exited $rc"
  cat "$work/copy.err" >&2 || true
else
  landed=""
  for _ in $(seq 1 40); do
    if [ "$(xclip -selection clipboard -o -t UTF8_STRING 2>/dev/null)" = "$copied" ]; then
      landed=1
      break
    fi
    sleep 0.25
  done
  if [ -n "$landed" ]; then
    pass "a copy on the server lands on the laptop's clipboard"
  else
    fail "the copy never reached the laptop's clipboard (found: $(xclip -selection clipboard -o -t UTF8_STRING 2>/dev/null | head -c 120))"
  fi
fi

echo
echo "-- what the session does while the channel is up (the baseline)"

# The whole task DAG, evaluated on the server against the stand-in riabuild-web.
# This is the "setup" half of the degradation promise, and it is recorded here
# so the run after the tunnel dies has something to be identical to. A dry run
# rather than a real one because applying would download a Node toolchain into a
# container this test throws away.
rc=0
timeout 180 docker exec "${server_env[@]}" "$CONTAINER" \
  /home/shared/riabuild --check >"$work/check-up.log" 2>&1 || rc=$?
if [ "$rc" -ne 0 ]; then
  fail "riabuild --check on the server exited $rc with the channel up"
  cat "$work/check-up.log" >&2 || true
fi
baseline="$(docker exec -u shared "$CONTAINER" \
  sh -c 'tail -n 1 ~/.riabuild/logs/riabuild.log' 2>/dev/null | awk '{print $4, $5}')"
case "$baseline" in
  satisfied=*applied=\[*\])
    pass "the setup flow ran on the server and logged $baseline"
    ;;
  *)
    fail "riabuild --check wrote no usable run log with the channel up (got '$baseline')"
    ;;
esac
# A baseline of `applied=[]` would make the comparison below true no matter what
# the channel did. These three name the setup, the checkout and the secrets
# task, so "the same" means "the same real work".
for task in toolchain project env_local; do
  case "$baseline" in
    *"$task"*) ;;
    *) fail "the baseline run did not evaluate the $task task: $baseline" ;;
  esac
done

# The environment shell, driven over a pipe. It is not a TTY, so bash says so on
# stderr and carries on; what matters is that the shell starts, sources
# riabuild's environment and reports it.
#
# `$BROWSER` is in the probe because it is the one variable whose absence is
# silent: `shell::environment` exports it for any session that inherits a
# non-empty `RIABUILD_CHANNEL_SOCKET`, and without it Claude Code's login URL
# renders in a terminal browser over the developer's own session. It went
# unset on every real server for exactly as long as `browser_for` read only
# `Ctx::env` — the channel up, the socket right, and every unit test green.
# Single-quoted on purpose: these are the *server's* variables, and expanding
# them here would send the shell a line of constants and assert nothing.
# shellcheck disable=SC2016
shell_probe='echo PROBE RIABUILD_SHELL=$RIABUILD_SHELL PATH_HEAD=${PATH%%:*} BROWSER=[$BROWSER]; exit'
# The shell echoes its own input back after the prompt, so the probe's *output*
# is the line that begins at column one. Matching anywhere would find the echo,
# which contains the unexpanded `$BROWSER` and would pass whatever happened.
probe_line() {                    # probe_line <log file>
  tr -d '\r' < "$1" | grep -m1 '^PROBE ' || true
}
rc=0
printf '%s\n' "$shell_probe" | timeout 120 docker exec -i "${server_env[@]}" \
  "$CONTAINER" /home/shared/riabuild shell >"$work/shell-up.log" 2>&1 || rc=$?
probe_up="$(probe_line "$work/shell-up.log")"
if [ "$rc" -eq 0 ] && [ -n "$probe_up" ]; then
  pass "the environment shell opens on the server with the channel up"
else
  fail "the environment shell did not open with the channel up (exit $rc)"
  cat "$work/shell-up.log" >&2 || true
fi
case "$probe_up" in
  *"BROWSER=[/home/shared/.riabuild/bin/xdg-open]"*)
    pass "the session exports BROWSER, so a login URL opens on the laptop"
    ;;
  *)
    fail "BROWSER did not point at the browser shim: $probe_up"
    ;;
esac

echo
echo "-- the tunnel dies mid-session"

kill "$tunnel_pid" >/dev/null 2>&1 || true
wait "$tunnel_pid" 2>/dev/null || true
tunnel_pid=""

# The socket node sshd left behind stays on disk, so this is the realistic
# failure: a path that exists and refuses connections, not a missing file.
channel_down=""
for _ in $(seq 1 40); do
  if ! on_server channel status >"$work/status-down.log" 2>&1; then
    channel_down=1
    break
  fi
  sleep 0.5
done
if [ -n "$channel_down" ] && grep -q "Clipboard channel — down" "$work/status-down.log"; then
  pass "riabuild channel status exits non-zero and names the channel as down"
else
  fail "the channel still answered after its tunnel was killed"
  cat "$work/status-down.log" >&2 || true
  exit 1
fi

# The failure this bounds is not a wrong answer, it is no answer: a paste that
# blocks on a dead socket wedges the terminal it was typed into, and that is the
# "environment broken" outcome the whole design forbids. `timeout` exits 124,
# which is a distinct failure from the exit 1 an empty clipboard produces.
rc=0
timeout 20 docker exec "${server_env[@]}" "$CONTAINER" \
  /home/shared/riabuild channel shim xclip -selection clipboard -t TARGETS -o \
  >"$work/down-targets.out" 2>"$work/down-targets.err" || rc=$?
if [ "$rc" -eq 124 ]; then
  fail "a paste with the channel down hung rather than returning"
elif [ "$rc" -eq 1 ] && [ ! -s "$work/down-targets.out" ]; then
  pass "a paste degrades to an empty clipboard — exit 1, nothing on stdout"
else
  fail "a paste with the channel down exited $rc with $(wc -c <"$work/down-targets.out") bytes of output"
fi

# A read that fails is indistinguishable from an empty clipboard on purpose. A
# write has no such twin: reporting success would lose what the developer
# copied, silently.
rc=0
printf 'this must not be silently dropped' \
  | timeout 20 docker exec -i "${server_env[@]}" "$CONTAINER" \
    /home/shared/riabuild channel shim xclip -selection clipboard -i \
    >"$work/down-copy.out" 2>"$work/down-copy.err" || rc=$?
if [ "$rc" -eq 124 ]; then
  fail "a copy with the channel down hung rather than returning"
elif [ "$rc" -ne 0 ]; then
  pass "a copy fails loudly rather than pretending it reached the laptop"
else
  fail "a copy with the channel down reported success"
fi

echo
echo "-- and everything that is not the clipboard keeps working"

rc=0
timeout 180 docker exec "${server_env[@]}" "$CONTAINER" \
  /home/shared/riabuild --check >"$work/check-down.log" 2>&1 || rc=$?
after="$(docker exec -u shared "$CONTAINER" \
  sh -c 'tail -n 1 ~/.riabuild/logs/riabuild.log' 2>/dev/null | awk '{print $4, $5}')"
if [ "$rc" -ne 0 ]; then
  fail "riabuild --check on the server exited $rc with the channel down"
  cat "$work/check-down.log" >&2 || true
elif [ "$after" = "$baseline" ]; then
  pass "setup re-runs with the channel dead, evaluating exactly the same tasks"
else
  fail "the channel's death changed what setup did: '$baseline' became '$after'"
fi
# The run reached riabuild-web, so "setup re-ran" is not just "riabuild started
# and gave up". This is the same round trip that re-pulls rotated secrets.
if grep -q "signed in as Ada Lovelace" "$work/check-down.log"; then
  pass "the server still reached riabuild-web and resolved its member with the channel dead"
else
  fail "the run with the channel down never got an answer from riabuild-web"
  cat "$work/check-down.log" >&2 || true
fi

rc=0
printf '%s\n' "$shell_probe" | timeout 120 docker exec -i "${server_env[@]}" \
  "$CONTAINER" /home/shared/riabuild shell >"$work/shell-down.log" 2>&1 || rc=$?
probe_down="$(probe_line "$work/shell-down.log")"
if [ "$rc" -ne 0 ] || [ -z "$probe_down" ]; then
  fail "the environment shell did not open with the channel down (exit $rc)"
  cat "$work/shell-down.log" >&2 || true
else
  pass "the environment shell still opens with the channel dead"
  # riabuild's own bin at the head of PATH is what makes `node`, `pnpm` and
  # `claude` resolve to the versions riabuild installed. A channel failure that
  # cost a developer that would be exactly the "environment broken" outcome, so
  # this is asserted against the value rather than only against the comparison
  # below — which would also be satisfied by both shells being equally broken.
  case "$probe_down" in
    *"PATH_HEAD=/home/shared/.riabuild/bin"*)
      pass "riabuild's bin still leads PATH in that shell"
      ;;
    *)
      fail "PATH lost riabuild's bin: $probe_down"
      ;;
  esac
  # The degradation promise stated exactly: the environment the session hands a
  # developer is the same one it handed them while the laptop was answering.
  if [ "$probe_down" = "$probe_up" ]; then
    pass "the shell's environment is unchanged by the channel dying"
  else
    fail "the channel's death changed the shell's environment:"
    fail "  with the channel up:   $probe_up"
    fail "  with the channel down: $probe_down"
  fi
fi

echo
if [ "$failures" -ne 0 ]; then
  echo "clipboard channel e2e: $failures assertion(s) failed." >&2
  exit 1
fi

# Names what was checked rather than claiming "all assertions passed" — the same
# reason `run.sh` spells its five out.
cat <<'SUMMARY'
clipboard channel e2e: passed.

  Carried, against a real xclip on a real X display and a real shim on the
  server: TARGETS for text and for an image, a UTF-8 string and a PNG pasted
  byte for byte, and a copy made on the server landing on the laptop.

  Degraded, with the tunnel killed mid-session: status says down, a paste
  returns an empty clipboard rather than hanging, a copy fails loudly, and
  setup, riabuild-web, the environment shell and its whole environment are
  exactly what they were while the laptop was still answering.

  Not covered here, and not by run.sh either yet: remote mode's own wiring
  (the supervisor's tunnel, RIABUILD_CHANNEL_SOCKET in the env prefix, the
  banner line) and a real secrets re-pull. See the header.
SUMMARY
