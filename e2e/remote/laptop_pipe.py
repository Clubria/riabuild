#!/usr/bin/env python3
"""The laptop's end of the exec channel, for `channel.sh`.

WHAT THIS STANDS IN FOR, AND WHY IT HAS TO.

Since the 2026-08-13 exec-transport design, the channel is one
`ssh -T <host> riabuild channel pump`. The pump binds `<namespace>/channel.sock`
on the server and multiplexes every shim connection onto its own stdio; the
laptop answers those frames from an in-process `Agent`
(`channel::agent::pipe::serve_pipe`), driven by `channel::supervisor`.

There is no way to reach that laptop half from a command line. `serve_pipe` has
exactly one caller — `supervisor::supervise` — and the supervisor has exactly
one caller — `remote::channel::open_shell` — which is reached only by a
complete `riabuild remote` run: a real published release downloaded and
verified, a session minted, the whole task DAG applied on the server. That is
`run.sh`'s territory and `run.sh` cannot get there yet. `riabuild channel
agent` still exists and still serves a real `Agent`, but over a **unix socket**,
one request per connection — not over a frame pipe.

So this script is the frame layer and nothing else: it reads frames from the
pump, hands each payload to the real `riabuild channel agent` over its real
socket, and frames the real answer back. Every byte of clipboard behaviour on
the laptop — `clipboard::detect`, the `xclip` argv, the PNG round trip, the
protocol allowlist — is riabuild's own code, unmodified. What is not covered by
`channel.sh` while this exists is `serve_pipe` itself and `supervisor`'s argv,
and the header of `channel.sh` says so out loud rather than leaving it to be
discovered.

It is written against the format the design document specifies and `mux.rs`
implements:

    {"id":7,"len":1234}\\n<1234 bytes>

An independent reader of a documented wire format is the same bargain
`stub_web.py` makes with `/api/v1`: if the two disagree, that is worth a red
test, because the format is the thing both ends have to agree on.

Two frames are special, and both come from the pump:

  * `id: 0` is the keepalive (`mux::KEEPALIVE_ID`). It carries no payload and
    asks nothing; it has to be answered, because a pump that goes
    `KEEPALIVE_DEADLINE` unanswered concludes the laptop is gone, unbinds the
    socket and exits. Answering it is how this script proves the keepalive
    cycle works; refusing to (`--deaf`) is how `channel.sh` proves the pump
    really does give the socket back.
  * any other id scopes one shim connection, and two of them may be in flight
    at once — hence a thread per frame and a lock on the pipe.

Usage:
    laptop_pipe.py --socket <agent.sock> [--stats <file>] [--deaf-after <n>]
                   -- <command to run as the pump, e.g. ssh -T host riabuild …>

Standard library only, for the same reason as `stub_web.py`.
"""

import argparse
import json
import os
import socket
import subprocess
import sys
import threading

# `protocol::MAX_PAYLOAD`. A frame announcing more than this is refused before
# anything is allocated, exactly as `mux::read_frame` refuses it — a stand-in
# that would happily reserve four gigabytes on being asked to is not standing
# in for the thing under test.
MAX_PAYLOAD = 32 * 1024 * 1024

KEEPALIVE_ID = 0


class Pipe:
    """One frame at a time on the wire, whoever is writing."""

    def __init__(self, writer):
        self.writer = writer
        self.lock = threading.Lock()

    def send(self, frame_id, payload):
        header = json.dumps({"id": frame_id, "len": len(payload)}).encode("utf-8")
        with self.lock:
            try:
                self.writer.write(header + b"\n" + payload)
                self.writer.flush()
            except (BrokenPipeError, ValueError):
                # The pump is gone. Nothing to report: the reader loop is about
                # to see the same end of pipe and stop.
                pass


def read_exactly(reader, count):
    """`count` bytes, or None if the pipe ended inside them.

    A short read is an error rather than a short frame: half a screenshot
    returned as if it were whole is the one failure worse than no paste, which
    is the same reasoning `mux::read_frame` gives for it.
    """
    chunks = []
    remaining = count
    while remaining:
        chunk = reader.read(remaining)
        if not chunk:
            return None
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def read_frame(reader):
    """`(id, payload)`, or None at a clean end of pipe."""
    line = reader.readline()
    if not line:
        return None
    header = json.loads(line.decode("utf-8"))
    length = header["len"]
    if length > MAX_PAYLOAD:
        raise ValueError(f"a frame announced {length} bytes, over the channel limit")
    payload = read_exactly(reader, length) if length else b""
    if payload is None:
        raise ValueError("the pipe ended inside a frame")
    return header["id"], payload


def ask_the_agent(agent_socket, payload):
    """One request to the real `riabuild channel agent`, one answer back.

    One connection per request, because that is what `agent::server` serves:
    it reads the request line (and a write's body), answers, and drops the
    stream. Reading to end of file is therefore the whole reply, header and
    body, with no need for this script to parse either — the same division of
    labour the pump has on the far end.
    """
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.connect(agent_socket)
        stream.sendall(payload)
        stream.shutdown(socket.SHUT_WR)
        answer = bytearray()
        while True:
            chunk = stream.recv(65536)
            if not chunk:
                break
            answer.extend(chunk)
        return bytes(answer)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--socket", required=True, help="the laptop agent's unix socket")
    parser.add_argument("--stats", help="where to write what this connection carried")
    parser.add_argument(
        "--deaf-after",
        type=int,
        default=-1,
        help="stop answering after this many keepalives, to prove the pump "
        "gives the socket back when its laptop stops answering",
    )
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()

    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("no pump command given")

    child = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=None,
        bufsize=0,
    )
    pipe = Pipe(child.stdin)
    counts = {"requests": 0, "keepalives": 0}
    counted = threading.Lock()
    workers = []

    def record():
        """What this connection has carried so far, on disk.

        Written after every frame rather than once at the end, because the
        thing `channel.sh` does to this connection is kill it — and a count
        that only appears on a clean exit is a count that is never there when
        it is wanted. Through a temporary and `os.replace`, so a reader never
        sees half a file.
        """
        if not args.stats:
            return
        temporary = args.stats + ".partial"
        with open(temporary, "w", encoding="utf-8") as handle:
            json.dump(counts, handle)
        os.replace(temporary, args.stats)

    record()

    while True:
        try:
            frame = read_frame(child.stdout)
        except (ValueError, json.JSONDecodeError) as error:
            # Unrecoverable by construction: the stream position is unknown, so
            # every later frame would be read out of the middle of this one.
            sys.stderr.write(f"laptop_pipe: {error}\n")
            break
        if frame is None:
            break
        frame_id, payload = frame

        if frame_id == KEEPALIVE_ID:
            with counted:
                counts["keepalives"] += 1
                deaf = 0 <= args.deaf_after < counts["keepalives"]
            record()
            if deaf:
                # Deliberately no answer. `pump::keepalive` gives up
                # KEEPALIVE_DEADLINE after the last frame it heard and unbinds
                # the socket, which is the recovery the exec transport exists
                # for and is not observable any other way.
                sys.stderr.write("laptop_pipe: going deaf, as asked\n")
                continue
            pipe.send(KEEPALIVE_ID, b"")
            continue

        with counted:
            counts["requests"] += 1
        record()

        def answer(frame_id=frame_id, payload=payload):
            try:
                pipe.send(frame_id, ask_the_agent(args.socket, payload))
            except OSError as error:
                # The agent is not there. Closing without a reply is what the
                # shim reads as "no channel", which is the truth.
                sys.stderr.write(f"laptop_pipe: agent: {error}\n")

        worker = threading.Thread(target=answer, daemon=True)
        worker.start()
        workers.append(worker)

    for worker in workers:
        worker.join(timeout=5)
    try:
        child.stdin.close()
    except OSError:
        pass
    child.terminate()
    record()


if __name__ == "__main__":
    main()
