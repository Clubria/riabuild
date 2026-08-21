#!/usr/bin/env bash
# The clipboard channel, end to end: a real laptop clipboard, a real
# `riabuild channel pump` over a real `ssh -T`, and a real shim on a real
# server.
#
# THE TRANSPORT IS AN EXEC SESSION, NOT A FORWARD, AND THIS FILE USED TO SAY
# OTHERWISE. Until 2026-08-21 the script stood up `ssh -N -R
# <remote.sock>:<laptop.sock>` and asserted against that — the streamlocal
# forward the 2026-08-13 exec-transport design *removed*, along with
# `ExitOnForwardFailure` and `StreamLocalBindUnlink`. Every assertion below
# passed, because sshd will still happily forward a unix socket to an agent
# that will still happily serve it; none of them touched `pump`, `mux`, the
# keepalive, or the socket rebind, which is the whole of what ships. A green
# tick read as "the channel is covered" for as long as that was true.
#
# What runs now is what remote mode runs: one `ssh -T`, whose remote command is
# the same `env 'K=V' … '/abs/riabuild' channel pump` that
# `remote::flow::connect` composes, with the connection's stdio *being* the
# channel. No forwarding permission is asked of the server for it — that is the
# point of the design, and a server that grants none is now a server this test
# resembles.
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
#    pump mid-session and holds the whole promise to account: setup re-runs
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
# THREE STAND-INS, ALL NAMED RATHER THAN HIDDEN.
#
# a. The server's riabuild is copied in rather than installed. `riabuild remote`
#    installs it by downloading a published release and verifying its digest,
#    and the release this branch's channel code is in has not been cut. So this
#    script copies in a locally built **musl** binary, which is the same target
#    a real install would fetch — and, unlike a released one, is the code under
#    test. What that skips is the install step, which `run.sh` owns; what it
#    exercises is everything after it.
#
# b. The shim is invoked as `riabuild channel shim xclip …` rather than through
#    `~/.riabuild/bin/xclip`. That generated file is a one-line
#    `exec riabuild channel shim xclip "$@"` (`shims::write_clipboard_shims`),
#    and nothing writes it yet — the function is `#[allow(dead_code)]`, waiting
#    on remote mode's wiring. So this runs what the wrapper would exec, one
#    `exec` short of the whole path, on the real server binary.
#
# c. THE LAPTOP END OF THE PIPE IS `laptop_pipe.py`, NOT `serve_pipe`. This is
#    the new one, and it is the honest cost of testing the exec transport from
#    a shell. On a real laptop the frames coming back up the pipe are answered
#    by `channel::agent::pipe::serve_pipe`, holding an `Agent` in process,
#    driven by `channel::supervisor`. `serve_pipe` has exactly one caller and
#    the supervisor has exactly one caller — `remote::channel::open_shell`,
#    reached only by a complete `riabuild remote` run — so there is no command
#    line that reaches it, and this script exists precisely because that run
#    cannot be had here.
#
#    So `laptop_pipe.py` is the frame layer and nothing else: it reads the
#    documented `{"id":N,"len":M}\n<bytes>` frames off the pump, hands each
#    payload to the real `riabuild channel agent` over its real socket, and
#    frames the real answer back. The clipboard, the argv, the atoms, the
#    allowlist and the whole of `protocol` are riabuild's own code. An
#    independent reader of a documented wire format is the same bargain
#    `stub_web.py` makes with `/api/v1`.
#
# WHAT THIS SCRIPT DOES NOT COVER, so that `run.sh` is not assumed to.
#
#   * `agent::pipe::serve_pipe` and `supervisor::ssh_args` — the laptop half of
#     the transport, per stand-in (c). `supervisor`'s argv is unit-tested
#     ("The supervisor spawns an exec session and never -R" in the design's own
#     test table) and `serve_pipe` is covered over an in-memory duplex. What
#     nothing covers is either of them against a real ssh, and this script's
#     `ssh -T … channel pump` is built to the same shape by hand so that the
#     *server's* half is genuinely exercised.
#   * Remote mode's own wiring — `src/remote/channel.rs`: the supervisor
#     holding the session up and rebuilding it, `RIABUILD_CHANNEL_SOCKET`
#     arriving in the `env 'K=V' … '/abs/riabuild'` prefix because remote mode
#     put it there rather than because a test did, the lease between two
#     terminals into one box, and the banner line. All of that exists in the
#     tree; none of it is observed here. The pump below is one this script owns
#     and can kill, deliberately: a supervisor that rebuilds a killed
#     connection is the right behaviour and the wrong thing to test a
#     degradation against. Observing it belongs to `run.sh`.
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

# Named for this run, never for this script. Distinct from `run.sh`'s container,
# image and ports was the old rule, and it was half of one: two copies of *this*
# script — two worktrees, or a developer poking at it while CI runs on the same
# self-hosted box — still shared one container name, one image tag and two
# fixed ports, and the cleanup below still ran `docker rm -f` on a name the
# other run answered to. The token comes from `mktemp -d`, already unique for
# this process.
token="$(basename "$work")"
CONTAINER="riabuild-e2e-channel-$token"
IMAGE="riabuild-e2e-channel:$token"

# Asked of the kernel rather than picked. A hard-coded 2223 is somebody's
# existing tunnel as often as it is free, and a second copy of this script
# would collide with the first on both numbers.
free_port() {
  python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()'
}
# Assigned after the `need` checks below, not here: `free_port` needs python3,
# and a missing one has to surface as the sentence naming what python3 is for
# rather than as an error out of a `$(...)`.
PORT="${CHANNEL_PORT:-}"
STUB_PORT="${CHANNEL_STUB_PORT:-}"
MEMBER="33333333-3333-4333-8333-333333333333"

# The namespace remote mode gives a developer on a shared server, and the socket
# inside it. Developers share one Unix account there, so they share one uid and
# one $XDG_RUNTIME_DIR — the namespace is what stops one developer's xclip
# reading another's laptop, and it is the path the pump binds.
remote_dir="/home/shared/.riabuild-remote/$MEMBER"
remote_sock="$remote_dir/channel.sock"
# The laptop agent's own socket. Under the exec transport nothing forwards this
# anywhere: `laptop_pipe.py` connects to it from this machine, and the only
# thing crossing the network is the ssh session's stdio.
laptop_sock="$work/channel.sock"

xvfb_pid=""
agent_pid=""
pump_pid=""
api_pid=""
stub_pid=""

# Ends the pump connection, both halves of it.
#
# `$pump_pid` names a process *group* — `start_pump` turns job control on for
# exactly that launch — so the negative pid reaches `laptop_pipe.py` and the
# `ssh` it spawned together. Killing only the python would leave the ssh alive
# with its pipes closed, and the pump on the far end of it holding
# `channel.sock` until its own keepalive deadline expired forty-five seconds
# later, which is a race every assertion after it would inherit.
stop_pump() {
  [ -n "$pump_pid" ] || return 0
  kill -TERM -"$pump_pid" 2>/dev/null || kill -TERM "$pump_pid" 2>/dev/null || true
  for _ in $(seq 1 20); do
    kill -0 -"$pump_pid" 2>/dev/null || break
    sleep 0.25
  done
  kill -KILL -"$pump_pid" 2>/dev/null || true
  pump_pid=""
}

cleanup() {
  # Xvfb first: every xclip holding a selection is a client of it, and killing
  # the server reaps them all. Matching them by name instead would be a `pkill
  # -f xclip` on a machine that may be somebody's desktop.
  # Unset and already-dead are both fine here: `kill` complains and `|| true`
  # swallows it. A guard would only make the list five times as long, and the
  # trap runs on every exit path including the ones that failed early.
  for pid in "$xvfb_pid" "$agent_pid" "$api_pid" "$stub_pid"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
  # The pump connection is a group, not a process: `laptop_pipe.py` and the
  # `ssh` it spawned. Killing only the python would leave an ssh holding the
  # server's socket open for as long as it took to notice its pipes had gone.
  stop_pump
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  docker rmi -f "$IMAGE" >/dev/null 2>&1 || true
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
need ssh "The channel is one \`ssh -T <host> riabuild channel pump\`, which is the thing under test."
need ssh-keygen "This script generates the key the container will trust."
need ssh-keyscan "The container's host key is read with it, so the SSH above can be strict."
need Xvfb "The laptop side needs a real X display for a real xclip to talk to — see the header."
need xclip "The agent drives it to read and write this laptop's clipboard, and detect refuses without it."
need python3 "It runs stub_web.py and laptop_pipe.py — the stand-in dashboard and the laptop end of the pipe."

[ -n "$PORT" ] || PORT="$(free_port)"
[ -n "$STUB_PORT" ] || STUB_PORT="$(free_port)"

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
#
# This is the list `remote::identity::ssh_options` stands for, and it is
# deliberately the *only* thing shared between the channel connection and the
# API tunnel below — `supervisor::ssh_args` takes exactly such a list from its
# caller and adds `-T`, the keepalives and `BatchMode` itself.
#
# `ExitOnForwardFailure` is no longer in it. It was one of the three options
# the 2026-08-13 exec-transport design removed along with `-R`, and leaving it
# on the channel's own connection would say this test still asks a server for
# a forwarding permission. It moves to the API tunnel, which really does
# forward and really does need to fail loudly when it cannot.
ssh_opts=(
  -F /dev/null
  -p "$PORT"
  -i "$work/key"
  -o UserKnownHostsFile="$work/known_hosts"
  -o StrictHostKeyChecking=yes
  -o IdentitiesOnly=yes
)

echo "-- installing the server's riabuild"
docker cp "$SERVER_BIN" "$CONTAINER:/home/shared/riabuild" >/dev/null
docker exec "$CONTAINER" chown shared:shared /home/shared/riabuild
docker exec "$CONTAINER" chmod 755 /home/shared/riabuild
docker exec -u shared "$CONTAINER" mkdir -p "$remote_dir"
# Left over from the forward this used to be, and kept for the opposite
# reason. Under `ssh -R` **sshd** called `bind()`, so a leftover socket was
# fatal and unclearable — `StreamLocalBindUnlink` is a server setting that
# defaults to `no` and the client option riabuild passed could not touch it.
# The pump binds the socket itself now and clears a dead one as its owner, so
# this line no longer has to be here for the channel to come up. It stays
# because a container that has just been created should not have one, and a
# socket here at *this* point would mean something is wrong with the image.
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
# kills the channel's pump, and sharing one process would take the API down
# with it and prove nothing about the channel at all.
#
# THIS `-R` IS NOT THE TRANSPORT THAT WAS DELETED, and the distinction is worth
# one sentence because the two look identical on a command line. What the
# exec-transport design removed was `streamlocal-forward@openssh.com` — the
# *unix-domain* remote forward, an OpenSSH extension a hardened server may
# refuse and a non-OpenSSH server may never have implemented — and it removed
# it from riabuild. This is a plain TCP remote forward, it belongs to the
# harness rather than to riabuild, and its only job is to give a container with
# no route to this machine's loopback a riabuild-web to talk to. Nothing under
# test asks a server for it.
ssh -N -R "$STUB_PORT:127.0.0.1:$STUB_PORT" -o ExitOnForwardFailure=yes \
  "${ssh_opts[@]}" shared@localhost >"$work/api-tunnel.log" 2>&1 &
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

# THE TRANSPORT. One `ssh -T`, whose remote command is the pump.
#
# `pump_command` is built to the shape `remote::flow::connect` composes —
# `env_command(&prefix, &binary, &["channel", "pump"])` — rather than to
# something convenient. In particular the socket is named by
# `RIABUILD_CHANNEL_SOCKET` in the prefix and not by a `--socket` flag: on a
# box several developers share, a pump that resolved the path for itself would
# give every one of them the same `channel.sock`, and Ada's xclip would read
# Ben's laptop. `remote::env_prefix`'s own doc calls that load-bearing, so
# testing it any other way would be testing something else.
pump_command="env 'RIABUILD_ROOT=$remote_dir' 'RIABUILD_REMOTE=e2e' \
'RIABUILD_CHANNEL_SOCKET=$remote_sock' 'CLOUDCLI_NO_TMUX=1' \
'/home/shared/riabuild' channel pump"

# The argv `supervisor::ssh_args` builds, by hand: `-T` first because the
# framing is binary and a pty would translate newlines and eat a 0x03 in the
# middle of a screenshot; the caller's options; the two keepalives that turn a
# black-hole network into an exit; `BatchMode` because nothing is watching.
# No `-R`, no `ExitOnForwardFailure`, no `StreamLocalBindUnlink`.
start_pump() {                    # start_pump [extra laptop_pipe args...]
  rm -f "$work/pipe-stats.json"
  # Job control for this launch only, so `$!` names a group: `laptop_pipe.py`
  # and the ssh it spawns have to die together. See `stop_pump`.
  set -m
  python3 "$here/laptop_pipe.py" \
    --socket "$laptop_sock" \
    --stats "$work/pipe-stats.json" \
    "$@" \
    -- ssh -T "${ssh_opts[@]}" \
         -o ServerAliveInterval=15 \
         -o ServerAliveCountMax=3 \
         -o BatchMode=yes \
         shared@localhost "$pump_command" \
    >>"$work/pipe.log" 2>&1 &
  pump_pid=$!
  set +m
}

# What the *current* connection has carried so far, as `laptop_pipe.py` has
# recorded it. Read through a function because two sections below ask, and
# because `start_pump` resets the file: a count is always about the connection
# running now, never a total for the run.
carried() {                       # carried <requests|keepalives>
  if [ ! -f "$work/pipe-stats.json" ]; then
    echo 0
    return
  fi
  python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))[sys.argv[2]])' \
    "$work/pipe-stats.json" "$1" 2>/dev/null || echo 0
}

# Poll the server's own probe rather than the socket file: the pump creates the
# node before it is serving, so its existence is not the channel being up. This
# is also the first assertion — `channel status` is what a developer runs to
# find out why paste stopped.
wait_for_channel() {              # wait_for_channel <log file>
  for _ in $(seq 1 40); do
    if on_server channel status >"$1" 2>&1; then
      return 0
    fi
    sleep 0.5
  done
  return 1
}

echo "-- starting the pump on the server, over ssh -T"
start_pump
channel_up=""
# An `if`, not `wait_for_channel … && channel_up=1`: under `set -e` an AND-list
# whose left side fails is itself a failed command, so the shorter spelling
# would end the run here instead of reaching the message below that says what
# went wrong.
if wait_for_channel "$work/status-up.log"; then channel_up=1; fi

echo
echo "-- the channel carries the clipboard"

if [ -n "$channel_up" ] && grep -q "Clipboard channel — connected" "$work/status-up.log"; then
  pass "riabuild channel status on the server reports the channel connected"
else
  fail "the channel never came up on the server"
  cat "$work/status-up.log" >&2 || true
  cat "$work/pipe.log" >&2 || true
  # Nothing below can mean anything without a channel, and a run that reports
  # eleven failures for one cause hides the cause.
  exit 1
fi

# The socket the pump bound is the one the prefix named, and it is inside the
# developer's namespace rather than in a runtime directory the whole account
# shares. Asserted rather than assumed: a pump that fell back to
# `socket_path`'s `$XDG_RUNTIME_DIR` guess would still answer `channel status`
# in *this* test, because there is only one developer in it — and would hand
# every developer on a real shared box the same socket.
if docker exec -u shared "$CONTAINER" test -S "$remote_sock"; then
  pass "the pump bound the namespaced socket the env prefix named"
else
  fail "no socket at $remote_sock — the pump chose its own path"
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
# The marker is split across a string concatenation so that the *command* the
# shell echoes back contains `PRO""BE` and only the *output* ever contains
# `PROBE`. That is what lets the match below ignore where on the line it lands.
#
# An earlier version anchored `^PROBE ` instead, reasoning that the echo is
# preceded by a prompt and the output is not. That is true only when the pty
# happens to flush the prompt, the echo and the output in that order — and it
# does not always: a failing run showed
# `(riabuild) shared@…:/$ PROBE RIABUILD_SHELL=1 …`, the right answer on the
# wrong column, with the echoed line half-overwritten behind it. Position on a
# pty is a race; the presence of a marker the input cannot contain is not.
# shellcheck disable=SC2016
shell_probe='echo "PRO""BE RIABUILD_SHELL=$RIABUILD_SHELL PATH_HEAD=${PATH%%:*} BROWSER=[$BROWSER]"; exit'
probe_line() {                    # probe_line <log file>
  tr -d '\r' < "$1" | grep -m1 -o 'PROBE .*' || true
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
echo "-- a second pump does not take the socket from a live one"

# Two terminals into one box, and the one thing that must not happen is the
# second silently cutting the first's paste. `pump::bind` connects to the
# socket to tell a live pump from a dead one — the file looks identical either
# way — and refuses a live one by name. Remote mode's lease is what makes this
# rare in production; `bind` is what makes it safe when the lease loses a race,
# and only a real second pump against a real live socket can show it.
rc=0
timeout 30 docker exec "${server_env[@]}" "$CONTAINER" \
  /home/shared/riabuild channel pump </dev/null \
  >"$work/second-pump.out" 2>&1 || rc=$?
if [ "$rc" -eq 0 ]; then
  fail "a second pump took over a socket another pump was serving"
elif [ "$rc" -eq 124 ]; then
  fail "a second pump neither bound nor refused — it hung"
elif grep -qi "already serving" "$work/second-pump.out"; then
  pass "a second pump is refused while the first is serving, and says why"
else
  fail "a second pump failed for some other reason (exit $rc):"
  cat "$work/second-pump.out" >&2 || true
fi
# And the first one is still the one serving.
if on_server channel status >"$work/status-still-up.log" 2>&1 \
  && grep -q "Clipboard channel — connected" "$work/status-still-up.log"; then
  pass "the pump that was already serving still is"
else
  fail "the refused pump cost the live one its socket"
  cat "$work/status-still-up.log" >&2 || true
fi

echo
echo "-- the pump dies mid-session, hard, leaving its socket behind"

# `kill -9` on the *server's* pump rather than a `kill` on this laptop's ssh,
# and the difference is the whole of what makes the next section mean
# anything. A pump that ends because its pipe closed unlinks the socket on the
# way out; one that is killed cannot, and leaves exactly the corpse the
# exec-transport design exists to make survivable — a path that exists and
# refuses connections. Under `ssh -R` that corpse was permanent, because
# **sshd** owned the bind and `StreamLocalBindUnlink` defaults to `no`.
#
# The pattern is split so this `sh`'s own command line does not contain the
# string it is matching on, which would otherwise have it kill itself halfway
# through the loop. Same trick as `shell_probe` above, same reason.
docker exec -u shared "$CONTAINER" sh -c '
  for entry in /proc/[0-9]*; do
    cmd=$(tr "\0" " " < "$entry/cmdline" 2>/dev/null) || continue
    case "$cmd" in
      *"channel pu""mp"*) kill -9 "${entry#/proc/}" 2>/dev/null ;;
    esac
  done' || true

# The laptop's end goes with it: the remote command died, so `ssh` exits, so
# `laptop_pipe.py` reaches the end of its pipe. Reaped here rather than left
# for the trap, because the next section starts a second one and two would
# both be trying to bind.
stop_pump

if docker exec -u shared "$CONTAINER" test -e "$remote_sock"; then
  pass "the killed pump left its socket behind, which is the case that used to be fatal"
else
  # Not a failure of riabuild: a clean unlink is the better outcome, it is just
  # not the one the assertion below is about. Said out loud so a reader is not
  # left thinking the stale-socket path was exercised when it was not.
  echo "  note the socket was cleaned up anyway; the rebind below is untested this run"
fi

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
  fail "the channel still answered after its pump was killed"
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
echo "-- what that first connection turned out to have carried"

# `laptop_pipe.py` counts the frames it answered, and the requests are the
# clipboard traffic above: their count is what says the frames really crossed
# the pipe rather than the shim having found some other way to an answer.
#
# THE KEEPALIVES ARE COUNTED HERE AND DELIBERATELY NOT ASSERTED HERE, which is
# the mistake this section shipped with. `pump::keepalive` sleeps one
# KEEPALIVE_INTERVAL — fifteen seconds — *before* it sends anything, and this
# connection is killed about a second and a half in, deliberately, because
# every assertion above it is about a session whose laptop has just gone. Zero
# keepalives on a connection that short is the shipped schedule working
# exactly as `pump/keepalive.rs` and the exec-transport design describe it, so
# demanding one here was an assertion that could never be anything but red —
# and "fixing" it by having the pump send a frame the moment the pipe opens
# would be fitting the product to the test, and would turn
# `Served::keepalives` from "the connection came up *and stayed up*" into
# merely "it came up". The keepalive is asserted where it can actually be
# observed: on the deaf pump at the bottom, which lives past three intervals.
carried_requests="$(carried requests)"
if [ "$carried_requests" -ge 5 ]; then
  pass "the clipboard traffic crossed the pipe as frames ($carried_requests of them)"
else
  fail "only $carried_requests request frames reached the laptop; the shim answered from somewhere else"
fi

echo
echo "-- and the channel comes back over the socket the dead one left"

# The failure the exec transport was designed to end, end to end. Under
# `ssh -R` the stale `channel.sock` above disabled paste on that server
# permanently and no riabuild flag could clear it, because the bind belonged to
# sshd. The pump owns it now, so clearing a dead one is an ordinary `unlink` by
# its owner — and the proof is that a second connection over the same path
# works, not that a function returned Ok.
start_pump
recovered=""
if wait_for_channel "$work/status-back.log"; then recovered=1; fi
if [ -n "$recovered" ] && grep -q "Clipboard channel — connected" "$work/status-back.log"; then
  pass "a new pump replaced the stale socket and the channel is up again"
else
  fail "the channel did not come back over the socket the killed pump left"
  cat "$work/status-back.log" >&2 || true
  cat "$work/pipe.log" >&2 || true
fi

# Not just up — carrying. A `channel status` that passes and a paste that does
# not is the difference between a socket being bound and a channel working.
printf 'after the rebind ✓' > "$work/rebind.src"
put_on_clipboard UTF8_STRING "$work/rebind.src"
rc=0
on_server channel shim xclip -selection clipboard -t UTF8_STRING -o \
  >"$work/rebind.out" 2>"$work/rebind.err" || rc=$?
if [ "$rc" -eq 0 ] && cmp -s "$work/rebind.src" "$work/rebind.out"; then
  pass "and it pastes: the laptop's clipboard reaches the server again"
else
  fail "the rebuilt channel answered status but carried nothing (exit $rc)"
  cat "$work/rebind.err" >&2 || true
fi

echo
echo "-- a pump whose laptop stops answering gives the socket back"

# The slowest assertion here — a little over KEEPALIVE_DEADLINE, 45 seconds —
# and the one that cannot be had any other way. `--deaf-after 1` has
# `laptop_pipe.py` stop answering keepalives while leaving the pipe open, which
# is exactly what a laptop on a flaky link looks like from the server: the TCP
# connection is not closed, it is simply never acknowledged again. Without the
# deadline the pump sits there bound to the socket for as long as the kernel
# retransmits.
#
# What is asserted is the *return*: the socket is unbound. That is what lets
# the reconnecting supervisor's own pump take the path, and what turns a paste
# into an immediate failure instead of a twenty-second wait for a laptop that
# is not there.
stop_pump
docker exec -u shared "$CONTAINER" rm -f "$remote_sock" 2>/dev/null || true
start_pump --deaf-after 1
deaf_up=""
if wait_for_channel "$work/status-deaf.log"; then deaf_up=1; fi
if [ -z "$deaf_up" ]; then
  fail "the pump for the keepalive test never came up"
  cat "$work/pipe.log" >&2 || true
else
  gave_it_back=""
  # 90 attempts at a second each: KEEPALIVE_INTERVAL (15s) to the first
  # unanswered frame, then KEEPALIVE_DEADLINE (45s) of silence, then the
  # unlink. Generous rather than tight — this is a timeout being waited out,
  # and a flaky assertion about a timing constant is worse than a slow one.
  for _ in $(seq 1 90); do
    if ! docker exec -u shared "$CONTAINER" test -e "$remote_sock"; then
      gave_it_back=1
      break
    fi
    sleep 1
  done
  if [ -n "$gave_it_back" ]; then
    pass "the pump gave the socket back once its laptop stopped answering"
  else
    fail "the pump held $remote_sock after its laptop went silent"
    cat "$work/pipe.log" >&2 || true
  fi

  # And it gave the socket back because it was *measuring*, not because it
  # happened to end. The unbind above proves the deadline fired; on its own it
  # does not prove a single frame was ever sent, because `pump::keepalive`
  # returns on the deadline whether or not its `try_send` ever reached the
  # pipe. What proves the send is the laptop having received the frames — the
  # half that had no coverage at all until this round, and the half that cost a
  # real outage: before the pump measured its laptop, one that dropped off left
  # the pump bound to `channel.sock` for as long as the kernel kept
  # retransmitting, during which every paste timed out, every reconnecting pump
  # was refused with `already serving`, and the supervisor called a server it
  # had reached on every attempt unreachable.
  #
  # This costs no wall clock: the poll above has already waited the cycle out.
  #
  # Two, and neither one nor three. `--deaf-after 1` answers the first
  # keepalive and ignores every one after it, so the pump sends at 15s, 30s and
  # 45s and returns at 60s without sending a fourth — three arrive. Two is that
  # with a whole interval of slack, and it still says the thing worth saying:
  # the pump went on measuring after the first silence rather than sending once
  # and stopping, which one cannot distinguish. Three would be tighter and
  # would go red on a runner whose fifteen-second sleeps ran at twenty-three,
  # and a flaky assertion about a timing constant is worse than a slow one.
  # That the answered frame is what *extends* the deadline is pinned on a
  # paused clock by `pump::tests::a_laptop_that_answers_keepalives_keeps_its_pump`,
  # where it costs nothing and cannot be flaky.
  answered="$(carried keepalives)"
  if [ "$answered" -ge 2 ]; then
    pass "the pump measured the laptop with its keepalive ($answered frames reached it)"
  else
    fail "only $answered keepalive frames reached the laptop, so nothing bounds how long a pump can outlive it"
    cat "$work/pipe.log" >&2 || true
  fi
fi
stop_pump

echo
if [ "$failures" -ne 0 ]; then
  echo "clipboard channel e2e: $failures assertion(s) failed." >&2
  exit 1
fi

# Names what was checked rather than claiming "all assertions passed" — the same
# reason `run.sh` spells its five out.
cat <<'SUMMARY'
clipboard channel e2e: passed.

  Carried, over a real `ssh -T … riabuild channel pump` with a real xclip on a
  real X display at one end and a real shim at the other: TARGETS for text and
  for an image, a UTF-8 string and a PNG pasted byte for byte, and a copy made
  on the server landing on the laptop. The pump bound the namespaced socket its
  env prefix named, and refused a second pump that wanted it.

  Degraded, with the pump killed mid-session: status says down, a paste returns
  an empty clipboard rather than hanging, a copy fails loudly, and setup,
  riabuild-web, the environment shell and its whole environment are exactly
  what they were while the laptop was still answering.

  Recovered: the clipboard traffic really crossed the pipe as frames, a new
  pump replaced the socket the killed one left behind and pasted over it, and a
  pump whose laptop stopped answering went on sending keepalives into the
  silence — counted at the laptop — and then unbound the socket rather than
  sitting on it.

  Not covered here, and not by run.sh either yet: the laptop half of the
  transport (`agent::pipe::serve_pipe` and `supervisor`, stood in for by
  `laptop_pipe.py`), remote mode's own wiring (the supervisor's connection, the
  lease, RIABUILD_CHANNEL_SOCKET arriving because remote mode put it there, the
  banner line) and a real secrets re-pull. See the header.
SUMMARY
