#!/usr/bin/env python3
"""A minimal stand-in for riabuild-web's /api/v1 contract.

`riabuild remote` talks to two real things — riabuild-web (for who you are
and what the org expects) and GitHub (for org membership and, eventually,
the release itself) — and this test cannot stand up a real Convex
deployment. `RIABUILD_TOKEN` (see `keychain.rs`'s own doc comment: "For CI
and for end-to-end tests against a local riabuild-web") lets the CLI skip
its browser-based login entirely and go straight to asking whichever
`RIABUILD_API_URL` it was given who that token belongs to — which is the one
seam this script exists to fill, in the fewest lines that make it real
rather than mocked: no Convex, no auth library, just the endpoints
`riabuild remote` and the server-side setup run actually read.

Standard library only, deliberately: the CI job that runs this has no
Node/pnpm setup step, and python3 ships on the `ubuntu-latest` runner image.

Usage: stub_web.py <port> <members-json-path>

`<members-json-path>` maps bearer token -> the member payload `/api/v1/me`
should hand back for it, e.g.:

    {"test-token-ada": {"githubLogin": "ada", "memberId": "...", ...}}

A ROUTE THIS DOES NOT IMPLEMENT SAYS SO IN ONE LINE, ON PURPOSE.
Every unhandled request logs `UNIMPLEMENTED <method> <path>` to stderr before
it answers, and `run.sh` reads that line rather than reading riabuild's own
error text. The difference matters: from the CLI's side "this dashboard has
no such route" and "this dashboard is broken" look identical, and the whole
job of `run.sh`'s `known_gap` is to never mistake one for the other. A gap in
*this file* is a fact this file states; anything else is riabuild's failure
and is fatal over there.

There is no stock `501` left to lean on. `BaseHTTPRequestHandler` answers an
unimplemented *method* with one, and for as long as `do_POST` was missing
that 501 was matched as a tracked gap — which meant `run.sh` exited 0 before
its assertions, and would have gone on doing so if riabuild had started
POSTing somewhere else entirely.
"""

import http.server
import json
import sys
import threading
import time
import uuid


def load_members(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


class Handler(http.server.BaseHTTPRequestHandler):
    members = {}
    # Tokens this stub minted for a server, as `token -> parent token`. Kept
    # apart from `members` so `delegate` can refuse a second hop the way
    # `convex/sessions.ts` does: a session that was itself delegated cannot
    # delegate. Guarded because `ThreadingHTTPServer` really does serve two
    # laptops at once here — `run.sh` runs as ada and then as bob.
    delegated = {}
    lock = threading.Lock()

    def log_message(self, format, *args):  # noqa: A002 - stdlib signature
        sys.stderr.write("stub_web: " + (format % args) + "\n")

    def _unimplemented(self, method):
        """Answer, and say in one line that the gap is this file's."""
        sys.stderr.write(f"stub_web: UNIMPLEMENTED {method} {self.path}\n")
        sys.stderr.flush()
        self._error(
            404,
            "not_found",
            f"stub_web has no route for {method} {self.path}",
            "Add it to e2e/remote/stub_web.py.",
        )

    def _json(self, status, body):
        payload = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _error(self, status, code, message, action):
        self._json(status, {"error": {"code": code, "message": message, "action": action}})

    def _bearer(self):
        header = self.headers.get("Authorization", "")
        if not header.startswith("Bearer "):
            return None
        return header[len("Bearer ") :]

    def _member_for(self, token):
        """The member a token belongs to, following one delegation hop.

        `__version__` is a configuration key sharing this map, never a token,
        so it is refused here rather than being allowed to authenticate a
        request whose `Authorization` header happened to say `__version__`.
        """
        if not token or token == "__version__":
            return None
        member = self.members.get(token)
        if isinstance(member, dict):
            return member
        return None

    def do_GET(self):  # noqa: N802 - stdlib method name
        if self.path == "/api/v1/me":
            member = self._member_for(self._bearer())
            if member is None:
                self._error(
                    401,
                    "unauthenticated",
                    "That session is not one riabuild recognises.",
                    "Run `riabuild login` again.",
                )
                return
            self._json(200, {"member": member})
            return

        if self.path == "/api/v1/org/config":
            # `defaultProjectPath` is a retired field kept only so an old
            # CLI released before it was removed still parses this reply;
            # every CLI this test builds ignores it.
            self._json(
                200,
                {
                    "repoSlug": "Clubria/riabuild",
                    "defaultProjectPath": "",
                    "minCliVersion": "0.0.0",
                    # A real, published release — see run.sh for why this
                    # has to be one riabuild-cli's own `download.rs` can
                    # actually resolve a checksums file for.
                    "latestCliVersion": self.members.get("__version__", "0.0.0"),
                    "secretsUpdatedAt": 0,
                },
            )
            return

        if self.path == "/api/v1/org/claude-settings":
            # Not optional, and not only for the assertion that reads it: the
            # `org_settings` task's own `check()` calls this on every run, with
            # `?`, so a 404 here does not mean "no team settings" — it fails
            # the whole server-side `riabuild --no-shell`, one step before the
            # isolation assertions below ever run. This route was missing while
            # the sign-in gap kept the run from reaching it, which is exactly
            # the shape of hole that surfaces the day a gap closes.
            #
            # The marker is what makes the assertion mean anything: a file
            # riabuild wrote empty and a file it fetched are both valid JSON,
            # and only one of them carries this.
            self._json(
                200,
                {
                    "settings": {
                        "env": {"CLUBRIA_ORG": "1", "CLUBRIA_REMOTE_E2E": "1"},
                        "permissions": {"defaultMode": "bypassPermissions"},
                    },
                    "updatedAt": 1,
                },
            )
            return

        self._unimplemented("GET")

    def do_POST(self):  # noqa: N802 - stdlib method name
        # `POST /api/v1/cli/sessions` — a signed-in laptop signs a server in.
        #
        # This is the one endpoint the whole of `run.sh`'s gated block waited
        # on. Without it the POST got `BaseHTTPRequestHandler`'s stock 501,
        # `known_gap` forgave that, and the script exited 0 with fifteen
        # assertions below it that had never once run.
        #
        # `convex/http.ts` is the contract, and the three gates worth
        # reproducing are the three a wrong CLI could get past a laxer stub:
        # the caller must be authenticated, the caller must not itself be a
        # delegated session (one hop only, `sessions.delegate`), and the reply
        # carries `token`, `sessionId` and `expiresAt` under exactly those
        # names — `ServerSessionReply` renames all three, and a stub that
        # answered `session_id` would deserialise into nothing and be blamed
        # on Convex.
        #
        # What is deliberately *not* reproduced: the re-check of GitHub org
        # membership. It is real and it is load-bearing, and it is checked
        # against real GitHub by `run.sh` itself before the container is even
        # built — a second copy here would only be able to lie.
        if self.path == "/api/v1/cli/sessions":
            token = self._bearer()
            member = self._member_for(token)
            if member is None:
                self._error(
                    401,
                    "unauthenticated",
                    "That session is not one riabuild recognises.",
                    "Run `riabuild login` again.",
                )
                return
            with self.lock:
                if token in self.delegated:
                    # 403, never 401: the session is valid and staying valid,
                    # so re-authenticating would change nothing. Same status
                    # and same code as the real endpoint, because the CLI
                    # tells this apart from every other refusal.
                    self._error(
                        403,
                        "delegation_not_permitted",
                        "This machine's riabuild session was itself signed in by another "
                        "machine, so it cannot sign a third one in.",
                        "Run `riabuild remote` from your own laptop.",
                    )
                    return
                minted = f"delegated-{uuid.uuid4().hex}"
                # The server's own riabuild authenticates with this token, so
                # it has to resolve to the same member — that is what makes
                # the namespace on the server the one this developer owns.
                self.members[minted] = member
                self.delegated[minted] = token
            self._json(
                200,
                {
                    "token": minted,
                    "sessionId": str(uuid.uuid4()),
                    # Ninety days, in milliseconds, and the server's answer
                    # rather than something the CLI computes — `expires_soon`
                    # on the laptop reads exactly this number back.
                    "expiresAt": int(time.time() * 1000) + 90 * 24 * 60 * 60 * 1000,
                    "member": member,
                },
            )
            return

        self._unimplemented("POST")

    def do_DELETE(self):  # noqa: N802 - stdlib method name
        # `riabuild remote forget` is not exercised by this test (see its
        # module doc), but answering rather than hanging is what makes that
        # a deliberate scope decision instead of an accident.
        #
        # Answered as a real revocation, not as an unimplemented route: a
        # session this stub can no longer find is exactly what `DELETE
        # /api/v1/cli/sessions/<id>` returns for one already gone, and
        # `forget` treats that as success. Logging it as UNIMPLEMENTED would
        # tell `run.sh` this harness had a hole where it has a decision.
        self._error(404, "session_unknown", "not exercised by this test", "n/a")


def main():
    port = int(sys.argv[1])
    members = load_members(sys.argv[2])
    Handler.members = members
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.serve_forever()


if __name__ == "__main__":
    main()
