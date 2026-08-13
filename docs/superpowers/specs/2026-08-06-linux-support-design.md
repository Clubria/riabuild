# Linux support — Design

**Date:** 2026-08-06
**Status:** Approved
**Extends:** [`2026-08-04-riabuild-design.md`](2026-08-04-riabuild-design.md)

## Purpose

riabuild ships for macOS. This adds Linux, distributed through apt and dnf, without
changing what riabuild is: a provisioner that gets a developer from "accepted a GitHub
org invite" to "running Claude Code against our codebase with working secrets" without
making a single environment decision.

The original design called for "macOS for v1, Linux-shaped code" — path resolution and
keychain access behind traits from the first commit. That held. The Linux-shaped work
that remains is concentrated in three places: the tasks that shell out to Homebrew, the
release pipeline, and one default path.

## What is already Linux-ready

Worth stating, because it bounds the work.

| File | State |
|---|---|
| `download.rs` | `node_platform()` and `pnpm_asset()` already emit `linux-{arm64,x64}` |
| `keychain.rs` | `SecretToolKeychain` (libsecret) exists, is tested, and `for_platform` selects it |
| `api/auth.rs` | `open_browser` already falls back to `xdg-open` |
| `claude_profiles.rs` | installs Claude Code through the riabuild-owned `npm` — no platform branch |
| `shell/` | bash, zsh, and fish handling is POSIX throughout |
| `paths.rs` | `default_project_dir_on` takes the OS as a parameter, so both answers stay testable from either platform |

## Shape of the change

Three pull requests, in order. Each one leaves the tree better than it found it, and the
first ships value on macOS on its own.

| PR | Contents |
|---|---|
| **A** | riabuild owns `gh` and `infisical`, on both platforms |
| **B** | musl builds, `.deb` and `.rpm`, the apt and dnf repositories, Linux self-update |
| **C** | the Linux project path, install documentation, dashboard install instructions |

Linux is not usable without A, because both remaining Homebrew call sites are in the
provisioning path rather than the distribution path.

---

# A — riabuild owns `gh` and `infisical`

## The decision

riabuild already owns Node, pnpm, and Claude Code: it downloads them, verifies them
against a published digest, and keeps them under `~/.riabuild/`. `gh` and `infisical`
are the two exceptions, installed with `brew install` and — on a machine without
Homebrew — not installed at all, with a failure that tells the developer to go and set
up Homebrew first.

That exception is what makes Linux support awkward, because there is no `brew` to
substitute. The alternatives were adding GitHub's apt repository and Infisical's
Cloudsmith repositories with `sudo`, or telling the developer to install them by hand.
Both are worse than the rule the codebase already follows everywhere else.

**After this change, nothing on the developer's `PATH` is trusted.** riabuild owns every
tool it depends on, on every platform. Homebrew survives in exactly one place:
distributing riabuild itself on macOS.

## Layout

```
~/.riabuild/
  gh/2.97.0/bin/gh
  infisical/0.43.120/infisical
  bin/gh  bin/infisical          shims, the way pnpm is already shimmed
```

Versioned directories, so a version bump is an install beside the old one rather than an
overwrite of a binary that might be running.

## Pinned versions, not "latest"

The versions are constants compiled into the binary:

```rust
const GH_VERSION: &str = "2.97.0";
const INFISICAL_VERSION: &str = "0.43.120";
```

Not resolved from a `releases/latest` API call at install time. Same reasoning as the
task manifest: what riabuild installs onto a laptop should be versioned, auditable, and
distributed through a signed release, not decided by whatever upstream published this
morning. Bumping one is a code change with a `version()` bump beside it, which is what
makes every existing install converge on the new version.

It also keeps the machine reproducible. Two developers who ran `riabuild` a week apart
get the same `gh`, and a bug that reproduces on one reproduces on the other.

## What `check()` looks at

`check()` reads the owned binary path directly rather than going through
`runner.which()`. `~/.riabuild/bin` is only prepended to `PATH` inside the environment
shell, not during provisioning, so `which("gh")` finds the *system* `gh` or nothing —
neither of which is the thing being verified. `toolchain` already checks Node this way.

```
owned binary missing            → Needs("gh is not installed yet")
reports a different version     → Needs("gh reports X but riabuild pins Y")
otherwise                       → the existing gh auth and membership checks, unchanged
```

A developer with a system `gh` keeps their sign-in. `gh auth status` reads
`~/.config/gh/hosts.yml`, which any `gh` binary shares, so switching to the owned one is
invisible — no re-authentication, no lost token.

## The upstream facts this depends on

Each of these was verified against the live releases on 2026-08-06 rather than assumed.
They are recorded here because every one of them is a thing that looks obvious and is
wrong.

### `gh` publishes macOS as a zip

| Platform | Asset | Contains |
|---|---|---|
| Linux | `gh_2.97.0_linux_{amd64,arm64}.tar.gz` | `gh_2.97.0_linux_amd64/bin/gh` |
| macOS | `gh_2.97.0_macOS_{amd64,arm64}.zip` | `gh_2.97.0_macOS_arm64/bin/gh` |

There is no macOS tar.gz. `.pkg` is the only other macOS asset, and it is an installer
that writes to `/usr/local` with `sudo`.

Both archives put the binary at `<prefix>/bin/gh`, where the prefix is the asset name
without its extension — so one extraction routine handles both once the container format
is dealt with.

Note the capitalisation: `macOS`, not `darwin` or `macos`. Note also that the
architecture words are Go's (`amd64`, `arm64`), not Rust's (`x86_64`, `aarch64`).

### The Infisical CLI moved repositories

`Infisical/infisical` stopped publishing the CLI at `infisical-cli/v0.41.90`. The live
repository is **`Infisical/cli`**, tagged plainly (`v0.43.120`), with assets named
`cli_0.43.120_{darwin,linux}_{amd64,arm64}.tar.gz`.

Writing this against the old repository would pin a CLI a year out of date without
anything failing.

The tarball has the binary at the **root**, not under a prefix directory, alongside
`completions/`, `manpages/`, `LICENSE`, and `README.md`. It is ~56 MB.

### Infisical's checksums are in two files, and the obvious one is a decoy

| File | Contents |
|---|---|
| `cli_0.43.120_checksums.txt` | one line, for `windows_amd64` |
| `checksums.txt` | everything **except** darwin |
| `checksums-darwin.txt` | the two darwin tarballs |

The darwin builds are produced separately, presumably notarised on a macOS runner, and
their digests never land in the main file. A verified download that reads the checksum
file named after the release finds nothing on every platform riabuild ships.

So digest lookup takes a **list** of checksum URLs and tries each in turn:

```rust
pub async fn digest_from_any(urls: &[String], filename: &str) -> Result<String>
```

`gh` needs only one (`gh_2.97.0_checksums.txt` covers every platform); Infisical needs
both. Both files use the `<digest>  <filename>` format `download::digest_for` already
parses for Node's `SHASUMS256.txt`, so only the fetching changes.

**A missing digest is a hard failure, never a skipped verification.** An unverified
download of a credential tool is worse than no download.

## Zip extraction

`download.rs` gains a zip extractor beside the existing gzip+tar one, using the `zip`
crate with deflate only.

The alternatives were shelling out to `unzip`, which is not installed by default on
macOS Sequoia and later or on a minimal Linux image, or relying on macOS `tar` being
libarchive and accepting zips — a platform quirk that would make this the one archive
riabuild extracts through a subprocess it cannot unit-test. Neither is worth avoiding one
pure-Rust dependency.

The extractor is a peer of the tar one and shares its contract: the digest is verified
**before** anything is written to disk, a single named member is extracted, and the
result lands at a caller-chosen path.

Zip entries carry a path from an untrusted archive, so the extractor rejects any member
whose normalised path escapes the destination — the `zip-slip` class. The tar extractor
takes the same treatment in this change; it currently extracts a known member from a
known-good tarball, which is safe today and is not a property to leave resting on the
digest alone.

## Task changes

`github_cli` and `infisical_cli` keep their ids, their positions in the DAG, and their
existing checks. What changes:

- `install()` downloads and verifies instead of calling `brew`, on both platforms
- `check()` gains the owned-binary-and-version check ahead of the existing ones
- `version()` bumps to 2, forcing every existing install to converge on the owned copies
- the "install Homebrew from brew.sh" failure path is deleted

`infisical_cli`'s check keeps its floor comparison for the pinned version, and keeps the
rule it is built around: **no token is installed**, and the presence of a credential is
never part of "healthy".

## Shims

`shims/mod.rs` generates `~/.riabuild/bin/{gh,infisical}` pointing at the versioned
directories, the way `pnpm` already is. The environment shell's `PATH` therefore serves
the owned copies to the developer's own commands, not just to riabuild.

---

# B — Distribution

## Binaries: static musl

Targets `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, built natively on
`ubuntu-latest` and `ubuntu-24.04-arm`. Native builds on each architecture avoid a cross
linker entirely.

Fully static means no glibc floor, so one build runs on every distribution and release
rather than on everything newer than whatever the runner had. The failure this avoids is
`/lib/x86_64-linux-gnu/libc.so.6: version 'GLIBC_2.35' not found` on an older machine —
a message that says nothing about riabuild and cannot be fixed by the developer reading
it. It is the same reasoning that made riabuild own the Node tarball rather than drive
nvm: pay a little in the build to remove a class of failure from every laptop.

The existing TLS choice is what makes this possible. `rustls-tls-native-roots` has no
OpenSSL to link, and root loading is plain file reads from `/etc/ssl/certs`, which every
target distribution provides.

Each build is verified before it is packaged: `file` reports `statically linked`, `ldd`
reports `not a dynamic executable`, and the binary is executed to confirm it prints the
expected version. The arm64 runner runs the arm64 binary, so both are actually executed
rather than one being checked structurally.

## Packages

`packaging/debian/control.in` and `packaging/rpm/riabuild.spec.in`, rendered by the
release workflow the way `packaging/homebrew/riabuild.rb` already is, and validated on
every pull request by the existing `packaging` CI job. A template that would break
`apt install` fails before it merges rather than after a release is published.

```
usr/bin/riabuild
usr/share/doc/riabuild/copyright
```

`.deb` is assembled with `dpkg-deb --build --root-owner-group`, `.rpm` with `rpmbuild
-bb`. Packaging is architecture-agnostic once the binary exists, so all four packages are
built in one job on `ubuntu-latest` from the two uploaded binaries.

**Architecture names differ per format** and are the easiest thing to get quietly wrong:

| Rust target | deb | rpm |
|---|---|---|
| `x86_64-unknown-linux-musl` | `amd64` | `x86_64` |
| `aarch64-unknown-linux-musl` | `arm64` | `aarch64` |

**The date version needs no special handling.** `dpkg` compares version components
numerically, so `2026.08.06` equals `2026.8.6` and `2026.08.06.1` sorts above
`2026.08.06`. `rpmvercmp` splits into alphabetic and numeric runs and compares numeric
runs numerically, giving the same answers. Both agree with `version.rs`. A CI test
asserts this rather than trusting the paragraph.

## Repositories: GitHub Pages

This repository is already its own Homebrew tap. It becomes its own apt repository and
its own dnf repository too, served from GitHub Pages at `clubria.github.io/riabuild`.

The alternative was a hosted service such as Cloudsmith, which is less CI code and one
more account, one more token, and one more free-tier ceiling between a developer and an
install.

```
/clubria.gpg                                     dearmoured public signing key
/deb/dists/stable/InRelease                      clearsigned
/deb/dists/stable/Release  Release.gpg           detached signature
/deb/dists/stable/main/binary-amd64/Packages{,.gz}
/deb/dists/stable/main/binary-arm64/Packages{,.gz}
/deb/pool/main/r/riabuild/riabuild_<version>_<arch>.deb
/rpm/clubria.repo                                ready-made, dnf4 and dnf5 alike
/rpm/{x86_64,aarch64}/riabuild-<version>-1.<arch>.rpm
/rpm/{x86_64,aarch64}/repodata/                  repomd.xml signed detached
/index.html                                      the install instructions
```

Metadata is generated with `apt-ftparchive` and `createrepo_c`. RPMs are signed with
`rpmsign` in addition to the repository metadata, so `gpgcheck=1` and `repo_gpgcheck=1`
are both meaningful in `clubria.repo`.

`clubria.repo` is shipped as a file and installed by copying it, rather than through
`dnf config-manager`, whose spelling changed between dnf4 (`--add-repo`) and dnf5
(`addrepo --from-repofile=`). A `curl` into `/etc/yum.repos.d/` works on both and will
keep working.

### Keeping the branch bounded

The repository is published as a **single squashed commit force-pushed to an orphan
`gh-pages` branch**, holding only the newest five releases.

Both halves matter. Package files are ~5 MB each and there are four per release; an
ordinary commit history would put every version ever released into every clone of this
repository, permanently. Squashing keeps the branch one commit deep, and pruning keeps
that commit small. Five versions is enough that a developer who pinned an older one can
still install it, and few enough that the branch stays under ~100 MB.

Because the metadata is regenerated from whatever packages are present, pruning is
"delete the files and re-run the generators" rather than a separate bookkeeping step.

### Concurrency

The publish job needs its own group:

```yaml
concurrency:
  group: pages
  cancel-in-progress: false
```

The existing `release-${{ github.ref }}` group does **not** serialise this. Every release
has a different tag, so two releases in flight are in two different groups, and both
would force-push `gh-pages`. The second would win and the first would vanish from the
repository while remaining installed on the machines that already had it.

### Signing key

Two repository secrets, `PACKAGE_SIGNING_KEY` (ASCII-armoured private key) and
`PACKAGE_SIGNING_KEY_ID`, with `PACKAGE_SIGNING_KEY_PASSPHRASE` optional.

Without them the release still builds the packages and attaches them to the GitHub
release, and the publish job emits a `::warning::` saying nobody was offered them —
matching how `CONVEX_DEPLOY_KEY` already degrades. An unsigned apt repository is not a
lesser version of a signed one; `apt` refuses it outright unless every developer adds
`[trusted=yes]`, which trains them to accept unsigned packages from anywhere.

## Self-update

`update.rs` currently runs `brew upgrade clubria/tap/riabuild`. It becomes
platform-aware, and the decision is made by **asking which package manager owns the
running executable** rather than by guessing from which tools are installed:

| Owner of the running binary | Upgrade |
|---|---|
| `dpkg -S` resolves it | `sudo apt-get update && sudo apt-get install --only-upgrade -y riabuild` |
| `rpm -qf` resolves it | `sudo dnf upgrade -y riabuild` |
| macOS | `brew upgrade clubria/tap/riabuild` — unchanged, no sudo |
| nobody | print the command, never sudo |

The last row is the one that needs saying. A Fedora machine can have `apt` installed. A
riabuild built with `cargo` or unpacked from a tarball is owned by no package manager at
all, and running `sudo apt-get install riabuild` against it would either fail or install
a *second* riabuild at a different path — leaving the developer running the old one
forever while every upgrade reports success.

The executable is resolved through `/proc/self/exe` on Linux before being handed to
`dpkg -S`, so a symlinked or `PATH`-relative invocation still resolves to the real file.

### sudo is asked for, not assumed

```
A newer riabuild is available (2026.08.12). Update now? [Y/n]
```

Then the upgrade runs through `run_interactive`, so the sudo password prompt appears in
the developer's terminal directly after the sentence explaining what it is for. An
unannounced password prompt at startup reads as something having gone wrong.

Under `--quiet`, or when stdin is not a TTY, riabuild prints the command instead of
prompting — a prompt nobody can answer is a hang.

A declined **mandatory** upgrade (below `minCliVersion`) still stops, exactly as today.
An optional one carries on with the current version.

`RIABUILD_UPDATED=1` keeps guarding the re-exec against loops, unchanged.

---

# C — Paths, documentation, and the dashboard

## The default checkout path

```rust
fn default_project_dir_on(os: &str, home: &Path, repo_name: &str) -> PathBuf {
    match os {
        "macos" => home.join("Documents").join(ORG_DIR).join(repo_name),
        "linux" => home.join(ORG_DIR).join(repo_name),
        _ => home.join("code").join(repo_name),
    }
}
```

`~/Clubria/ai-builders-hub` on Linux — the same organisation grouping macOS uses under
`~/Documents`, minus the `Documents` directory Linux does not have. The repository name
comes from the org config's repo slug, as it already does, so this is not hardcoded to
one repository.

Other operating systems keep `~/code/<repo>`. The developer can still pass
`riabuild --project <path>`, and that choice is remembered.

## Install instructions

Debian and Ubuntu:

```sh
curl -fsSL https://clubria.github.io/riabuild/clubria.gpg \
  | sudo tee /usr/share/keyrings/clubria.gpg >/dev/null
echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/clubria.gpg] https://clubria.github.io/riabuild/deb stable main" \
  | sudo tee /etc/apt/sources.list.d/clubria.list >/dev/null
sudo apt update && sudo apt install riabuild
```

Fedora, RHEL, and derivatives:

```sh
sudo curl -fsSL -o /etc/yum.repos.d/clubria.repo \
  https://clubria.github.io/riabuild/rpm/clubria.repo
sudo dnf install riabuild
```

The dashboard's install panel becomes three tabs — Homebrew, apt, dnf — defaulting to
the visitor's platform read from `navigator.userAgent`, with the other two a click away
rather than hidden. The `Command` component already renders newlines with `pre-wrap` and
copies the whole string, so the three-line apt block is one paste.

`README.md`, `docs/releasing.md`, `CLAUDE.md`, and `riabuild-cli/CLAUDE.md` are updated
in the same change. `docs/releasing.md` gains the one-time signing key setup, written out
as the exact commands.

## The Homebrew formula keeps `depends_on :macos`

Homebrew runs on Linux, and the formula would install there. It should not: the Linux
binary is distributed through apt and dnf, self-update looks for the package manager that
owns the executable, and a brew-installed Linux riabuild would find none and never
upgrade itself. Leaving the constraint in place makes that a clear message at install
time rather than a machine that quietly stops updating.

---

# Testing

Nothing below needs a real machine in a real state, which is what keeps the suite alive.

| Layer | Approach |
|---|---|
| Owned-tool `check()` | fixture `~/.riabuild` trees and canned `--version` output, as every task already does |
| Asset naming | asserted against the exact strings captured from the live releases, per platform and architecture, so an upstream rename fails in CI |
| Checksum selection | Infisical darwin resolves from `checksums-darwin.txt`, linux from `checksums.txt`, and a digest found in neither is an error |
| Archive extraction | in-memory fixture zip and tar archives, including a member whose path escapes the destination |
| Update strategy | `dpkg -S` / `rpm -qf` output canned through `FakeRunner`, including the owned-by-nobody case |
| Package versions | `dpkg --compare-versions` and `rpmdev-vercmp` agree with `version.rs` across the date scheme and the fourth component |
| Rendered templates | `control` and `.spec` parsed on every pull request beside the formula, and asserted to point at the artefacts the release actually builds |
| Repository metadata | CI builds the repository, serves it locally, and runs a real `apt update && apt install riabuild` in a Debian container and `dnf install` in a Fedora container |

That last row is the one that earns its cost. A broken `Release` signature, a wrong
`Filename:` path, or an architecture mismatch produces a repository that looks perfectly
healthy in the workflow log and fails on the developer's first command.

# Not in scope

- Windows, and any other operating system
- musl-vs-glibc *choice* — riabuild ships static musl only
- Arch, Alpine, Nix, or Snap packaging
- Replacing Homebrew as the macOS distribution channel
- ~~A file-based fallback for machines with no keyring. `RIABUILD_TOKEN` already covers the
  headless case, and writing a token to `~/.riabuild/` would break an invariant the whole
  brokering design rests on.~~

  **Reversed.** Both halves of that reasoning were wrong, and the second only
  looked right because of the first.

  `RIABUILD_TOKEN` never covered the headless case. It is a CI and e2e hook —
  the string does not appear anywhere in riabuild-web, and the dashboard has no
  screen that shows a developer a token to copy. There was no way for a human to
  obtain the value this bullet told them to set. A developer who ran riabuild on
  a Linux server got a browser approval, a discarded token, and
  `secret-tool: Cannot autolaunch D-Bus without X11 $DISPLAY` presented as a
  riabuild bug to report to their team lead.

  And the invariant is not what this would have broken. What "no secrets in
  `~/.riabuild/`" protects is the **Infisical org credential**, which is still
  brokered per use and still never written down. A session token for one machine,
  at 0600, is the same object the remote-mode design already sanctioned writing to
  a server's namespace and the remote-password design already widened to a
  keyring-less laptop — both for the argument that applies here unchanged: the
  alternative is not "no token on disk", it is that riabuild does not run on that
  machine at all.

  A related mistake shipped alongside this one: "has this machine a keyring?" was
  implemented as `which("secret-tool")`. libsecret is a *client* for a D-Bus
  Secret Service, so the binary being present says nothing about a service being
  reachable — which is why the failure surfaced at the write rather than at the
  decision. `keychain::keyring_answers` now probes the service, and the
  "No secrets in `~/.riabuild/`" note in `riabuild-cli/CLAUDE.md` carries the rule.
