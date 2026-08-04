# Releasing riabuild

Cutting a release is two commands, and there is no one-time setup — this
repository is its own Homebrew tap, and the release workflow has write access
to it already.

```sh
# from a clean main, with riabuild-cli/Cargo.toml already bumped
git tag v0.2.0
git push origin v0.2.0
```

`.github/workflows/release.yml` then builds both macOS architectures, publishes
them as a GitHub release, and commits the rendered formula to
`Formula/riabuild.rb` on main. A developer picks it up with:

```sh
brew tap clubria/tap https://github.com/Clubria/riabuild   # first time only
brew install clubria/tap/riabuild
brew upgrade clubria/tap/riabuild                          # or riabuild does it itself
```

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

1. **Bump `version` in `riabuild-cli/Cargo.toml`** and merge it through a PR
   like any other change.
2. **Tag the merge commit** as `v<version>` and push the tag.
3. **Watch the run** — `gh run watch` — until both jobs are green.
4. **Set `latestCliVersion`** in the dashboard's lead panel to the new version.
   Until this happens the release exists but no CLI offers it, because the
   startup check reads the version from `/api/v1/org/config`, not from GitHub.
5. **Leave `minCliVersion` alone** unless you mean it. It is the floor below
   which the CLI refuses to run, and raising it interrupts whatever everyone is
   doing at the moment they next launch riabuild.

The tag and `Cargo.toml` must agree. The workflow checks this and fails the
release rather than shipping a binary that reports a different version than the
formula installed — that mismatch makes every launch run a `brew upgrade` that
cannot change anything.

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

**`Formula/riabuild.rb` is generated.** Edit `packaging/homebrew/riabuild.rb`;
the next release overwrites the generated copy. CI renders the template and
parses it with `ruby -c` on every pull request, so a template that would break
`brew install` fails before it is merged rather than after a release has
already been published.

## When something goes wrong

| Symptom | Cause |
|---|---|
| Release fails on "Resolve and check version" | The tag and `Cargo.toml` disagree. Delete the tag, fix the version, tag again. |
| `formula` job fails, release published | Usually a push race on main. Re-run the job; it rebases. |
| `brew install` reports 404 | The release assets did not upload, or the repository stopped being public. |
| `brew install` cannot find the formula | The developer skipped `brew tap`, or the formula has not landed on main yet. |
| `riabuild` installs but is killed on launch | A signing problem — check the `Sign` step ran. |
| Nobody is offered the new version | `latestCliVersion` was never updated in the lead panel. |

To withdraw a release, delete it and its tag, then revert `Formula/riabuild.rb`
on main. Anyone who already upgraded keeps the withdrawn build until the next
release, so publishing a higher version is usually the better move.
