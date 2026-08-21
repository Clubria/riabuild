#!/usr/bin/env bash
#
# Republishes Grok Build's four platform builds as a riabuild release, and prints
# the digests to paste into `riabuild-cli/crates/fetch/src/tools.rs`.
#
# riabuild owns every tool it installs, and owning one means pinning a version
# and verifying it against a digest. xAI publishes the first and not the second:
# `https://x.ai/cli/grok-1.0.5-linux-x86_64` names a real version and a version
# nobody published is a 404, so unlike ngrok there *is* an immutable-looking URL
# here — but there is no checksum file beside it at any spelling, and
# `x.ai/cli/install.sh` verifies nothing. It downloads the binary, runs
# `--version` against it, and installs it.
#
# So riabuild takes a copy of the bytes it verified and points at that. The
# alternative — pinning our own digest against xAI's URL — fails in the worse
# direction: a version re-cut under the same name becomes a checksum mismatch and
# a hard install failure on every laptop at once, for bytes nobody can fetch any
# more.
#
# The bytes are **not repacked**. xAI serves an uncompressed executable, and a
# tarball made here would mean the digest in `tools.rs` describes this script's
# output rather than what xAI served. The assets are renamed to `.bin` and
# nothing else — renaming a file does not change its contents — which is what
# lets `archive::Kind::of` still read the container off the name. See
# `Kind::Raw`.
#
# This is a *release* step, not a build step. A `tools.rs` naming a version
# nobody has mirrored yet is a 404 on every laptop, so publish the assets first
# and land the constants after.
#
#   ./packaging/grok/mirror.sh            # latest stable: download, report, upload
#   ./packaging/grok/mirror.sh 1.0.5      # a particular version
#   ./packaging/grok/mirror.sh --dry-run  # download and report only
#
# Design: docs/superpowers/specs/2026-08-21-grok-build-design.md

# The tag is `grok-v<version>`, which is not the `v<date>` shape riabuild's own
# releases use — checked, because `release.yml` builds the apt and dnf
# repositories from `gh release list --limit 50` filtered by
# `^v[0-9]{4}\.[0-9]{2}\.[0-9]{2}...$`. A mirror tag cannot match that filter,
# and `--latest=false` below keeps it from taking the badge Homebrew's users
# read. It does occupy one of those 50 rows, so mirror tags stay rare: one per
# Grok Build bump, which is a code change anyway.
#
# These assets are large — 134 to 167 MB each, about 588 MB a version — which is
# another reason not to mirror casually. GitHub's per-file limit is 2 GB, so the
# size is a bandwidth and housekeeping question rather than a hard one; prune old
# `grok-v*` tags once no released riabuild pins them.

set -euo pipefail

CHANNEL_BASE="https://x.ai/cli"
# The direct GCS origin `install.sh` falls back to when the Cloudflare-fronted
# x.ai is unreachable. Same bytes, same paths.
FALLBACK_BASE="https://storage.googleapis.com/grok-build-public-artifacts/cli"

# xAI's platform words are Rust's, not Go's: `x86_64`/`aarch64` and
# `macos`/`linux`. `tools.rs` matches on `std::env::consts` for exactly this
# reason, and the order here is the order of the `GROK_BUILDS` table there.
PLATFORMS=(
  "macos-aarch64"
  "macos-x86_64"
  "linux-aarch64"
  "linux-x86_64"
)

dry_run=false
target=""
for arg in "$@"; do
  case "$arg" in
    --dry-run) dry_run=true ;;
    -*) echo "unknown option $arg" >&2; exit 1 ;;
    *) target="$arg" ;;
  esac
done

command -v gh >/dev/null || { echo "gh is required to upload the release" >&2; exit 1; }
command -v shasum >/dev/null || { echo "shasum is required" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Pick a base that answers, the way install.sh does. The probe doubles as the
# channel-pointer fetch when no version was named.
pointer=""
BASE="$CHANNEL_BASE"
pointer="$(curl -fsSL "$CHANNEL_BASE/stable" 2>/dev/null || true)"
if [[ -z "$pointer" ]]; then
  echo "note: $CHANNEL_BASE unreachable, falling back to direct GCS." >&2
  BASE="$FALLBACK_BASE"
  pointer="$(curl -fsSL "$BASE/stable" 2>/dev/null || true)"
fi

if [[ -n "$target" ]]; then
  version="$target"
else
  version="$(printf '%s' "$pointer" | tr -d '[:space:]')"
fi
[[ -n "$version" ]] || { echo "could not determine a Grok Build version to mirror" >&2; exit 1; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9._]+)?$ ]] \
  || { echo "invalid version format: $version (expected X.Y.Z)" >&2; exit 1; }

echo "mirroring Grok Build $version from $BASE" >&2

for platform in "${PLATFORMS[@]}"; do
  echo "downloading grok $version $platform…" >&2
  # No extension upstream: the download *is* the executable.
  curl -fsSL -o "$work/grok-$version-$platform.bin" \
    "$BASE/grok-$version-$platform"
done

# Read the version back out of the build for the host we are on, which is the
# only one we can execute. A URL that names a version and serves another is
# exactly the ngrok failure this repository already knows about, and the whole
# mirror is worthless if the bytes are not what the name says.
host_os="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$host_os" in
  darwin) host_os="macos" ;;
  linux) host_os="linux" ;;
  *) echo "unsupported host OS $host_os" >&2; exit 1 ;;
esac
host_arch="$(uname -m)"
case "$host_arch" in
  arm64 | aarch64) host_arch="aarch64" ;;
  x86_64 | amd64) host_arch="x86_64" ;;
  *) echo "unsupported host architecture $host_arch" >&2; exit 1 ;;
esac
host="$host_os-$host_arch"

probe="$work/probe"
cp "$work/grok-$version-$host.bin" "$probe"
chmod +x "$probe"
# GROK_HOME is named because Grok Build *creates* the one it is pointed at, and a
# release script has no business conjuring the maintainer's `~/.grok`.
reported="$(GROK_HOME="$work/grok-home" "$probe" --version 2>&1 | head -1)"
echo "the $host build reports: $reported" >&2
case "$reported" in
  *"$version"*) ;;
  *) echo "the downloaded binary does not report $version — refusing to mirror it" >&2; exit 1 ;;
esac

tag="grok-v$version"
echo
echo "  pub const GROK_VERSION: &str = \"$version\";"
echo "  const GROK_MIRROR: &str = \"https://github.com/Clubria/riabuild/releases/download/$tag\";"
echo
echo "  const GROK_BUILDS: &[(&str, &str, &str)] = &["

# Collected once and printed three times: as the Rust table for the person
# pasting it into `tools.rs`, and into the release notes so anyone auditing the
# mirror can check it against xAI without cloning anything.
notes="$work/notes.md"
{
  echo "Grok Build $version, republished **unmodified** from <$CHANNEL_BASE> so riabuild can"
  echo "pin it and verify it against a digest committed to its own repository. **This is not"
  echo "a riabuild release** — see the \`v<date>\` tags for those."
  echo
  echo "Why this exists: xAI publishes no checksum for these artifacts, at any spelling, and"
  echo "its own \`install.sh\` verifies nothing — it downloads the binary, runs \`--version\`"
  echo "against it, and installs it. riabuild does not lower its bar to match, and it will not"
  echo "let a *server* choose the digest either, which would be the task manifest under"
  echo "another name."
  echo
  echo "The bytes are byte-for-byte what xAI served. The only change is the \`.bin\` suffix on"
  echo "the filename, which is how riabuild's \`archive::Kind\` knows the download is a bare"
  echo "executable rather than an archive; renaming a file does not change its contents, so"
  echo "each digest below is the digest of the upstream artifact."
  echo
  echo "| asset | upstream | sha256 |"
  echo "|---|---|---|"
} > "$notes"

for platform in "${PLATFORMS[@]}"; do
  asset="grok-$version-$platform.bin"
  digest="$(shasum -a 256 "$work/$asset" | awk '{print $1}')"
  os="${platform%-*}"
  arch="${platform#*-}"
  printf '      ("%s", "%s", "%s"),\n' "$os" "$arch" "$digest"
  printf '| `%s` | `%s` | `%s` |\n' \
    "$asset" "$CHANNEL_BASE/grok-$version-$platform" "$digest" >> "$notes"
done
echo "  ];"
echo

{
  echo
  echo "The same digests are the \`GROK_BUILDS\` table in"
  echo "\`riabuild-cli/crates/fetch/src/tools.rs\`, and \`tools::install\` refuses anything that"
  echo "does not match. Regenerate all of this with \`./packaging/grok/mirror.sh\`."
  echo
  echo "Verify any row yourself:"
  echo
  echo '```sh'
  echo "curl -fsSL $CHANNEL_BASE/grok-$version-linux-x86_64 | shasum -a 256"
  echo '```'
} >> "$notes"

if $dry_run; then
  echo "--dry-run: nothing uploaded." >&2
  exit 0
fi

if ! gh release view "$tag" >/dev/null 2>&1; then
  gh release create "$tag" \
    --title "Grok Build $version (mirrored)" \
    --notes-file "$notes" \
    --latest=false
else
  # A re-run after a partial upload should not leave last run's digests standing.
  gh release edit "$tag" --notes-file "$notes"
fi
gh release upload "$tag" "$work"/grok-"$version"-*.bin --clobber

echo "uploaded to $tag — now update tools.rs with the constants above." >&2
