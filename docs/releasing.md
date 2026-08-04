# Releasing riabuild

Cutting a release is one command against a clean main. There is nothing to
bump first and no one-time setup — this repository is its own Homebrew tap, and
the release workflow can already write to it.

```sh
git push origin "v$(date -u +%Y.%m.%d)"    # after: git tag "v$(date -u +%Y.%m.%d)"
```

`.github/workflows/release.yml` then builds both macOS architectures, publishes
them as a GitHub release, and commits the rendered formula to
`Formula/riabuild.rb` on main. A developer picks it up with:

```sh
brew tap clubria/tap https://github.com/Clubria/riabuild   # first time only
brew install clubria/tap/riabuild
brew upgrade clubria/tap/riabuild                          # or riabuild does it itself
```

## Versioning is by release date

A version is the UTC date it was released, zero-padded: **`2026.08.04`**. A
second release on the same day adds a fourth component — `2026.08.04.1`, then
`.2`. There is no major, minor, or patch, and no meaning attached to a version
beyond when it shipped.

**The git tag is the only place a version is written down.** Nothing is bumped
before tagging, and `riabuild-cli/Cargo.toml` holds a permanent `0.0.0`
placeholder that is never the product version.

That is not an accident of tooling, it is the reason the scheme works. Cargo
requires the `version` field to be valid semver, and semver forbids both the
leading zeros in `08` and a fourth component. So the release workflow injects
the tag as `RIABUILD_VERSION`, and `cli.rs` compiles that in instead of
`CARGO_PKG_VERSION`. A binary that reports a different version than the release
it shipped in stops being a mistake anyone can make.

Both version comparators — `riabuild-cli/src/version.rs` and
`riabuild-web/convex/lib/version.ts` — are plain dotted-numeric, so they handle
dates, zero padding, and the fourth component without any special cases.
`2026.08.04` and `2026.8.4` compare equal.

The workflow checks the tag's *shape* rather than its value. `v20260804` or
`v2026.8.4` would each publish a release that sorts wrongly against every other
one for the rest of the project's life, so both are rejected.

A local `cargo build` has no tag and reports **`9999.0.0-dev`** — deliberately
above every real date, so a development build clears whatever `minCliVersion`
the server enforces and never talks itself into running `brew upgrade` over the
binary being worked on.

## This repository is the tap

Homebrew will tap any git repository under a name you choose, provided the
formulae live in `Formula/`. `brew tap clubria/tap <url>` registers this
repository under the name `clubria/tap`, so `clubria/tap/riabuild` resolves
and, importantly, so does the `brew upgrade clubria/tap/riabuild` that
`update.rs` runs on its own.

The cost is the explicit `brew tap` line. `brew install clubria/tap/riabuild`
auto-taps only when Homebrew can derive the repository name from the tap name,
and the name it derives is `Clubria/homebrew-tap`. Collapsing install back to a
single command means a separate repository with that exact name; nothing else
about the pipeline would change.

Both the repository and the release assets are public, which is what makes
`brew install` work at all: it fetches with plain `curl`, carrying no GitHub
credentials. Nothing is lost by that — the binary contains no secrets, and org
membership, secret brokering, and org config are re-verified server-side on
every request. Someone outside the Clubria org who installs riabuild gets a
program that can only tell them they are not in the Clubria org.

## Cutting a release

1. **Tag a commit on main** with today's UTC date, and push the tag:
   ```sh
   git tag "v$(date -u +%Y.%m.%d)" && git push origin "v$(date -u +%Y.%m.%d)"
   ```
   Releasing twice in one day? Add a fourth component: `v2026.08.04.1`.
2. **Watch the run** — `gh run watch` — until all three jobs are green.

That is the whole procedure. The run builds and publishes the release, updates
`Formula/riabuild.rb`, and announces the version to riabuild-web so developers
are actually offered it.

**`minCliVersion` is never touched by any of this.** It is the floor below
which the CLI refuses to run, and raising it interrupts whatever everyone is
doing the moment they next launch riabuild. It is a deliberate decision made in
the dashboard's lead panel, not a consequence of shipping.

Use the UTC date, not your local one — that is what `date -u` above gives you,
and what the workflow suggests back if you mistype a tag. A tag dated a day
ahead of the previous release's is the only ordering that matters, but keeping
to UTC is what stops two people in different timezones disagreeing about which
day it is.

### Trying the pipeline without releasing

The workflow also runs on demand, defaulting to build-and-package only:

```sh
gh workflow run release.yml
```

That exercises the build, the signing, the smoke test, and the packaging, and
leaves the tarballs as run artifacts. Nothing is published and the formula is
not touched. Use it after editing the workflow.

## What the workflow does, and why

**Both architectures are built on one arm64 runner.** Apple's toolchain
cross-compiles within its own family, so `--target x86_64-apple-darwin` on an
arm64 runner is ordinary work rather than a cross-compilation setup. The arm64
binary is executed to confirm it reports the expected version; the x86_64 one
is checked with `file`, since the runner cannot run it, and then by the
formula's own `test do` block the first time Homebrew installs it on Intel.

**The binaries are re-signed after stripping.** `strip = "symbols"` in the
release profile can invalidate the ad-hoc signature rustc applies at link time.
Apple silicon refuses to execute a binary whose signature does not verify, and
reports it as `killed: 9` with nothing further — a failure that appears only on
a developer's machine, never in CI. `codesign --force --sign -` after stripping
makes the outcome independent of what the toolchain did.

**No Apple Developer ID and no notarization.** Homebrew does not apply the
quarantine attribute to formula downloads the way it does for casks, so
Gatekeeper never inspects the binary and an ad-hoc signature is sufficient.
Notarization would become necessary if riabuild were ever distributed as a
`.dmg`, a cask, or a direct download.

**The formula job checks out main, not the tag.** `brew tap` clones the default
branch and reads nothing else, so a formula committed onto the tag would never
be seen. It rebases before pushing, because anything merged while the macOS
build was running would otherwise reject the push.

**The version is announced, not just published.** The CLI reads what to upgrade
to from `/api/v1/org/config`, never from GitHub, so a GitHub release nobody has
been told about reaches nobody — silently, with nothing anywhere reporting a
problem. The `announce` job calls `release:publishCliVersion`, which re-checks
with GitHub that the release really exists before writing, and
`org.setLatestCliVersion` refuses to move the version backwards. Between them
the only value that can land there is the newest genuinely published build,
which is why the entry point needs no shared secret — a Convex deploy key
cannot write environment variables, so a secret could only have been installed
by hand.

Without `CONVEX_DEPLOY_KEY` the release still publishes and the job warns that
nobody was offered it.

**`Formula/riabuild.rb` is generated.** Edit `packaging/homebrew/riabuild.rb`;
the next release overwrites the generated copy. CI renders the template and
parses it with `ruby -c` on every pull request, so a template that would break
`brew install` fails before it is merged rather than after a release has
already been published.

## When something goes wrong

| Symptom | Cause |
|---|---|
| Release fails on "Resolve and check version" | The tag is not a zero-padded date. Delete it and tag `vYYYY.MM.DD`. |
| `riabuild --version` says `9999.0.0-dev` | A binary built without `RIABUILD_VERSION` — a local `cargo build`, not a release. |
| `formula` job fails, release published | Usually a push race on main. Re-run the job; it rebases. |
| `brew install` reports 404 | The release assets did not upload, or the repository stopped being public. |
| `brew install` cannot find the formula | The developer skipped `brew tap`, or the formula has not landed on main yet. |
| `riabuild` installs but is killed on launch | A signing problem — check the `Sign` step ran. |
| Nobody is offered the new version | The `announce` job was skipped — `CONVEX_DEPLOY_KEY` is unset. |

To withdraw a release, delete it and its tag, then revert `Formula/riabuild.rb`
on main. Anyone who already upgraded keeps the withdrawn build until the next
release, so publishing a higher version is usually the better move.
