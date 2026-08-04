# Releasing riabuild

Cutting a release is two commands. Everything else on this page is either
one-time setup or an explanation of why a step exists.

```sh
# from a clean main, with riabuild-cli/Cargo.toml already bumped
git tag v0.2.0
git push origin v0.2.0
```

`.github/workflows/release.yml` then builds both macOS architectures, publishes
them as a GitHub release, and pushes an updated formula to
`Clubria/homebrew-tap`. A developer picks it up with:

```sh
brew install clubria/tap/riabuild     # first time
brew upgrade clubria/tap/riabuild     # afterwards, or riabuild does it itself
```

## One-time setup

Neither step can be done from this repository, because both need a credential
the repository does not contain.

### 1. Create the tap repository

Homebrew resolves `clubria/tap` to `github.com/Clubria/homebrew-tap`. The name
is not a convention you may vary — `brew` constructs it.

```sh
gh repo create Clubria/homebrew-tap --public \
  --description "Homebrew formulae for Clubria tools"
```

**It must be public.** `brew install` fetches the formula and the release
tarball with plain `curl`, carrying no GitHub credentials, so a private tap
returns 404 to every developer. This costs nothing: the binary contains no
secrets, and every gate that matters — org membership, secret brokering, org
config — is re-verified server-side on each request. Someone outside the
Clubria org who downloads riabuild gets a program that can only tell them they
are not in the Clubria org.

The first release creates `Formula/riabuild.rb`; the repository can start empty.

### 2. Add the `TAP_GITHUB_TOKEN` secret

The workflow's `GITHUB_TOKEN` is scoped to `Clubria/riabuild` alone, so pushing
to a second repository needs its own credential.

Create a fine-grained personal access token limited to `Clubria/homebrew-tap`
with **Contents: read and write**, then:

```sh
gh secret set TAP_GITHUB_TOKEN --repo Clubria/riabuild
```

If this is missing, the release itself still publishes correctly and only the
`tap` job fails — the tap is stale, not broken, and re-running the job after
adding the secret fixes it.

## Cutting a release

1. **Bump `version` in `riabuild-cli/Cargo.toml`** and merge it through a PR
   like any other change.
2. **Tag the merge commit** as `v<version>` and push the tag.
3. **Watch the run** — `gh run watch` — until the release and the tap job are
   both green.
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
leaves the tarballs as run artifacts. Nothing is published and the tap is not
touched. Use it after editing the workflow.

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

**The formula lives in `packaging/homebrew/riabuild.rb`.** The workflow
substitutes the version and both checksums and pushes the result. Edit that
template; a change made directly in the tap is overwritten by the next release.

## When something goes wrong

| Symptom | Cause |
|---|---|
| Release job fails on "Resolve and check version" | The tag and `Cargo.toml` disagree. Delete the tag, fix the version, tag again. |
| `tap` job fails, release published | `TAP_GITHUB_TOKEN` is missing or expired, or the tap repository does not exist. Fix it and re-run the job. |
| `brew install` reports 404 | The tap repository is private, or the release assets did not upload. |
| `riabuild` installs but is killed on launch | A signing problem — check the `Sign` step ran. |
| Nobody is offered the new version | `latestCliVersion` was never updated in the lead panel. |

To withdraw a release, delete it and its tag, then revert the formula in the
tap. Anyone who already upgraded keeps the withdrawn build until the next
release, so publishing a higher version is usually the better move.
