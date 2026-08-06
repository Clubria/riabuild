# Releasing riabuild

Cutting a release is one command against a clean main. There is nothing to
bump first — this repository is its own Homebrew tap, its own apt repository,
and its own dnf repository, and the release workflow can already write to all
three.

```sh
git push origin "v$(date -u +%Y.%m.%d)"    # after: git tag "v$(date -u +%Y.%m.%d)"
```

`.github/workflows/release.yml` then builds both macOS architectures and both
Linux ones, publishes them as a GitHub release, commits the rendered formula to
`Formula/riabuild.rb` on main, and rebuilds the apt and dnf repositories on
GitHub Pages. A developer picks it up with:

```sh
# macOS
brew tap clubria/tap https://github.com/Clubria/riabuild   # first time only
brew install clubria/tap/riabuild

# Debian, Ubuntu
curl -fsSL https://clubria.github.io/riabuild/clubria.gpg \
  | sudo tee /usr/share/keyrings/clubria.gpg >/dev/null
echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/clubria.gpg] https://clubria.github.io/riabuild/deb stable main" \
  | sudo tee /etc/apt/sources.list.d/clubria.list >/dev/null
sudo apt update && sudo apt install riabuild

# Fedora, RHEL
sudo curl -fsSL -o /etc/yum.repos.d/clubria.repo \
  https://clubria.github.io/riabuild/rpm/clubria.repo
sudo dnf install riabuild
```

riabuild upgrades itself from whichever of these installed it — see
[Self-update](#self-update-asks-the-package-manager-that-owns-the-binary).

## The signing key

apt refuses an unsigned repository outright, and the alternative — telling every
developer to write `[trusted=yes]` — trains them to accept unsigned packages
from anywhere. So the repositories are signed, and the key lives in two
repository secrets: `PACKAGE_SIGNING_KEY` (armoured private key) and
`PACKAGE_SIGNING_KEY_ID` (fingerprint).

Both are set, along with Pages serving from GitHub Actions. Nothing here needs
doing before the next release. This section is the rotation procedure.

```sh
gpg --batch --quick-gen-key --passphrase '' \
  'Clubria Package Signing <engineering@clubria.com>' rsa4096 sign never
gpg --list-secret-keys --with-colons | awk -F: '/^fpr:/ {print $10; exit}'   # the key id
gpg --armor --export-secret-keys <key id>                                    # the private key
```

Three things about that command are load-bearing, and the workflow rejects a key
that gets any of them wrong rather than failing somewhere less obvious.

**`rsa4096`, not `default`.** GnuPG 2.4 resolves `default` to ed25519, and
rpm 4.14 — RHEL 8 and every rebuild of it — cannot verify an ed25519 signature.
It reports `digests SIGNATURES NOT OK`, which `dnf` surfaces as a corrupt
package rather than as an unsupported algorithm, so the failure points at the
wrong thing entirely. RHEL 9 and Fedora both accept ed25519; RHEL 8 is the
cutoff, and it is supported until 2029. RSA is also what Fedora, EPEL, and
Docker sign with.

**No passphrase**, which is what `--passphrase ''` is for. `rpmsign` drives gpg
with no terminal attached, so a passphrase-protected key hangs the build waiting
for a pinentry that can never appear. The repository secret is what protects the
key; a passphrase sitting beside it in a second secret protects nothing and
takes the release down.

**`never` expires.** An expired repository key breaks `apt update` on every
installed machine at once, and there is no renewal step in this pipeline to
catch it coming.

Rotation is expensive, which is the reason for the care above: the key is
trusted by being pinned into `/usr/share/keyrings/clubria.gpg` and the rpm
keyring, so replacing it means every already-provisioned machine has to import
the new one by hand before it can update again. Keep the private key and its
revocation certificate in a password manager — a GitHub secret cannot be read
back, so the copy you keep is the only retrievable one.

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

## …and the apt and dnf repositories

Same idea, one layer out: the `pages` job generates a signed apt repository and
a signed dnf repository and publishes them to GitHub Pages at
`clubria.github.io/riabuild`.

They are **rebuilt from the newest five GitHub releases on every run**, never
appended to. That is what keeps this cheap. Nothing is stored between runs, so
there is no branch quietly accumulating every package ever released into every
clone of this repository, and a repository that somehow ends up corrupt is
repaired by re-running the job rather than by hand. Five is enough that someone
who pinned a recent version can still install it.

Signing is not optional and not a formality:

| File | Signed with | Why |
|---|---|---|
| `dists/stable/InRelease` | clearsigned | what modern apt reads |
| `dists/stable/Release.gpg` | detached | what older apt reads |
| `rpm/*/repodata/repomd.xml` | detached, armoured | `repo_gpgcheck=1` |
| each `.rpm` | `rpmsign` | `gpgcheck=1` |

Both rpm settings are on, and both mean something. `gpgcheck` alone would accept
a `repomd.xml` anyone could serve; `repo_gpgcheck` alone would accept any
package a valid index happened to point at.

`clubria.repo` is installed by copying the file rather than through `dnf
config-manager`, whose spelling changed between dnf4 (`--add-repo`) and dnf5
(`addrepo --from-repofile=`). A `curl` into `/etc/yum.repos.d/` works on both
and will keep working.

## Self-update asks the package manager that owns the binary

`update.rs` does not guess from which tools are installed. It asks `dpkg -S` and
then `rpm -qf` which package owns the running executable:

| Owner | Upgrade |
|---|---|
| dpkg | `sudo apt-get update && sudo apt-get install --only-upgrade riabuild` |
| rpm | `sudo dnf upgrade --refresh riabuild` |
| macOS | `brew upgrade clubria/tap/riabuild` — no sudo |
| nobody | prints the command, never sudoes |

The last row is the one that matters. A Fedora machine can have `apt` on it, and
a riabuild built with `cargo` or unpacked from a tarball is owned by nothing at
all — running `sudo apt-get install riabuild` against that would install a
*second* riabuild at a different path and leave the developer on the old one
forever, while every upgrade reported success.

The Linux paths need sudo, so riabuild asks first and then runs the upgrade
interactively, putting the password prompt directly after the sentence
explaining what it is for. Under `--quiet`, or with no terminal to ask at, it
prints the command instead: a prompt nobody can answer is a hang.

## Cutting a release

1. **Tag a commit on main** with today's UTC date, and push the tag:
   ```sh
   git tag "v$(date -u +%Y.%m.%d)" && git push origin "v$(date -u +%Y.%m.%d)"
   ```
   Releasing twice in one day? Add a fourth component: `v2026.08.04.1`.
2. **Watch the run** — `gh run watch` — until every job is green.

That is the whole procedure. The run builds and publishes the release, updates
`Formula/riabuild.rb`, rebuilds the apt and dnf repositories, and announces the
version to riabuild-web so developers are actually offered it.

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

That exercises the builds, the signing, the smoke tests, and the packaging —
including installing the real `.deb` and `.rpm` with a real apt and a real dnf
in containers — and leaves everything as run artifacts. Nothing is published,
the formula is not touched, and the repositories are not rebuilt. Use it after
editing the workflow.

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

**The Linux binaries are statically linked against musl.** One build runs on
every distribution and release, so there is no glibc floor to get wrong. The
failure this removes is `version GLIBC_2.35 not found` on an older machine: a
message that says nothing about riabuild and that the developer reading it
cannot act on. It is the same trade the Node tarball made — pay a little in the
build to remove a class of failure from every laptop.

`ring` is the only dependency with C in it, and cc-rs looks for a compiler
called `<arch>-linux-musl-gcc`, which Ubuntu's `musl-tools` does not provide; it
ships `musl-gcc`. So `CC_<target>` is set explicitly. Without it the build fails
outright with `failed to find tool`, which is at least loud — letting it fall
back to the host's glibc `cc` would be the quiet version of the same mistake.

**Each architecture is packaged on its own runner.** Not because packaging needs
a compiler — it does not — but because `rpmbuild --target aarch64` on an x86_64
host fails with `No compatible architectures found for build`, and so does
`rpmspec -P`, so the spec cannot even be parsed there. Packaging where the
binary was built sidesteps it, and has the useful side effect that the container
install test runs natively on both architectures.

**A static binary that builds and reports its version can still reach nothing.**
`rustls-tls-native-roots` has to find a certificate store, and musl's resolver
is not glibc's. Neither is visible without making a real request, so the Linux
job runs `tls_and_dns_work_on_this_build` — an otherwise-ignored test that
fetches a real URL over TLS — against the artefact it just built.

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
| No Linux developer is offered the version | The `pages` job warned and stopped — `PACKAGE_SIGNING_KEY` is unset. See [The signing key](#the-signing-key). |
| `pages` fails on "The signing key needs a passphrase" | The key in `PACKAGE_SIGNING_KEY` is passphrase-protected. Generate a passphrase-less one. |
| `pages` fails on "signing key is not RSA" | The key was generated with `default` rather than `rsa4096`. See [The signing key](#the-signing-key). |
| `dnf` reports the package is corrupt on RHEL 8, but installs fine on Fedora | An ed25519 signing key. rpm 4.14 cannot verify one and says so as a digest failure. Rotate to RSA. |
| `apt update` reports `NO_PUBKEY` or a bad signature | The signing key was rotated. Developers need the new `clubria.gpg`; the old keyring file is stale. |
| `apt install riabuild` says it is not found after a successful `apt update` | An architecture mismatch — check `[arch=…]` in the sources line matches `dpkg --print-architecture`. |
| `dnf` reports the repository is not signed | The `pages` job did not finish; `repomd.xml.asc` is missing. Re-run it. |
| Linux build fails with `failed to find tool` | `musl-tools` did not install, or `CC_<target>` was not set. |
| `riabuild` never updates itself on Linux | Nothing owns the binary — `dpkg -S $(command -v riabuild)` will say so. It was installed from a tarball, not a package. |

To withdraw a release, delete it and its tag, revert `Formula/riabuild.rb` on
main, and re-run the `pages` job on the previous tag so the repositories are
rebuilt without it. Anyone who already upgraded keeps the withdrawn build until
the next release, so publishing a higher version is usually the better move.
