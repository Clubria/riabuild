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
rather than mocked: no Convex, no auth library, just the two GET endpoints
`riabuild remote` actually reads before it ever touches SSH.

Standard library only, deliberately: the CI job that runs this has no
Node/pnpm setup step, and python3 ships on the `ubuntu-latest` runner image.

Usage: stub_web.py <port> <members-json-path>

`<members-json-path>` maps bearer token -> the member payload `/api/v1/me`
should hand back for it, e.g.:

    {"test-token-ada": {"githubLogin": "ada", "memberId": "...", ...}}
"""

import http.server
import json
import sys


def load_members(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


class Handler(http.server.BaseHTTPRequestHandler):
    members = {}

    def log_message(self, format, *args):  # noqa: A002 - stdlib signature
        sys.stderr.write("stub_web: " + (format % args) + "\n")

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

    def do_GET(self):  # noqa: N802 - stdlib method name
        if self.path == "/api/v1/me":
            token = self._bearer()
            member = self.members.get(token) if token else None
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

        self._error(404, "not_found", f"stub_web has no route for {self.path}", "n/a")

    def do_DELETE(self):  # noqa: N802 - stdlib method name
        # `riabuild remote forget` is not exercised by this test (see its
        # module doc), but answering rather than hanging is what makes that
        # a deliberate scope decision instead of an accident.
        self._error(404, "session_unknown", "not exercised by this test", "n/a")


def main():
    port = int(sys.argv[1])
    members = load_members(sys.argv[2])
    Handler.members = members
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.serve_forever()


if __name__ == "__main__":
    main()
