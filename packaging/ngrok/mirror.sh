#!/usr/bin/env bash
#
# Republishes ngrok's four platform builds as a riabuild release, and prints the
# digests to paste into `riabuild-cli/crates/fetch/src/tools.rs`.
#
# riabuild owns every tool it installs, and owning one means pinning a version
# and verifying it against a digest. ngrok offers neither: Equinox serves a
# single floating build per platform from one channel URL, and the version in
# that URL is decorative — `ngrok-v9.99.9-stable-linux-amd64.tgz` and
# `ngrok-v3-stable-linux-amd64.tgz` return the same bytes. So riabuild takes a
# copy of the bytes it verified and points at that instead.
#
# This is a *release* step, not a build step. A `tools.rs` naming a version
# nobody has mirrored yet is a 404 on every laptop, so publish the assets first
# and land the constants after.
#
#   ./packaging/ngrok/mirror.sh            # download, report, and upload
#   ./packaging/ngrok/mirror.sh --dry-run  # download and report only
#
# Design: docs/superpowers/specs/2026-08-18-ngrok-design.md

set -euo pipefail

CHANNEL="https://bin.equinox.io/c/bNyj1mQVY4c"
PLATFORMS=(
  "darwin-arm64 zip"
  "darwin-amd64 zip"
  "linux-arm64 tgz"
  "linux-amd64 tgz"
)

dry_run=false
[[ "${1:-}" == "--dry-run" ]] && dry_run=true

command -v gh >/dev/null || { echo "gh is required to upload the release" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# The version is read from the binary rather than passed in: the download is
# whatever Equinox is serving right now, and asking for a version we did not
# verify is the mistake this whole script exists to avoid.
version=""

for entry in "${PLATFORMS[@]}"; do
  read -r platform extension <<<"$entry"
  echo "downloading ngrok $platform…" >&2
  curl -fsSL -o "$work/$platform.$extension" \
    "$CHANNEL/ngrok-v3-stable-$platform.$extension"
done

# Read the version out of the build for the host we are on, which is the only
# one we can execute. The four are published together from one release, so one
# answer covers them — and if it ever does not, the digests will disagree with
# whatever a laptop downloads and `install` will refuse it rather than run it.
host_os="$(uname -s | tr '[:upper:]' '[:lower:]')"
[[ "$host_os" == "darwin" ]] && host_os="darwin"
host_arch="$(uname -m)"
case "$host_arch" in
  arm64 | aarch64) host_arch="arm64" ;;
  x86_64 | amd64) host_arch="amd64" ;;
  *) echo "unsupported host architecture $host_arch" >&2; exit 1 ;;
esac
host="$host_os-$host_arch"

mkdir -p "$work/probe"
if [[ -f "$work/$host.tgz" ]]; then
  tar xzf "$work/$host.tgz" -C "$work/probe"
else
  unzip -q "$work/$host.zip" -d "$work/probe"
fi
version="$("$work/probe/ngrok" version | awk '{print $NF}')"
[[ -n "$version" ]] || { echo "could not read a version out of the ngrok binary" >&2; exit 1; }
echo "ngrok reports version $version" >&2

tag="ngrok-v$version"
echo
echo "  pub const NGROK_VERSION: &str = \"$version\";"
echo "  const NGROK_MIRROR: &str = \"https://github.com/Clubria/riabuild/releases/download/$tag\";"
echo

for entry in "${PLATFORMS[@]}"; do
  read -r platform extension <<<"$entry"
  asset="ngrok-$version-$platform.$extension"
  mv "$work/$platform.$extension" "$work/$asset"
  digest="$(shasum -a 256 "$work/$asset" | awk '{print $1}')"
  printf '  %-34s %s\n' "$platform" "$digest"
done
echo

if $dry_run; then
  echo "--dry-run: nothing uploaded." >&2
  exit 0
fi

if ! gh release view "$tag" >/dev/null 2>&1; then
  gh release create "$tag" \
    --title "ngrok $version (mirrored)" \
    --notes "ngrok $version, republished unmodified from $CHANNEL so riabuild can pin it and verify it against a digest committed to this repository. Not a riabuild release." \
    --latest=false
fi
gh release upload "$tag" "$work"/ngrok-"$version"-*.{zip,tgz} --clobber

echo "uploaded to $tag — now update tools.rs with the constants above." >&2
