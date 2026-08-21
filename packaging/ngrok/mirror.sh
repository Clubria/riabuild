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
#   ./packaging/ngrok/mirror.sh 3.30.0            # download, report, and upload
#   ./packaging/ngrok/mirror.sh 3.30.0 --dry-run  # download and report only
#   ./packaging/ngrok/mirror.sh 3.30.0 --force    # replace assets already mirrored
#
# The version is an *argument* rather than something this script discovers. It
# used to unpack the host's download and run `ngrok version` to find out what it
# had — executing an unverified binary to decide what to trust, which is the one
# act the whole mirror exists to avoid on a laptop. Naming the version up front
# turns that execution into a check that can refuse: the host build is still run,
# but only to confirm it reports the version being asked for.
#
# Design: docs/superpowers/specs/2026-08-18-ngrok-design.md

# The tag is `ngrok-v<version>`, which is not the `v<date>` shape riabuild's own
# releases use — checked, because `release.yml` builds the apt and dnf
# repositories from `gh release list --limit 50` filtered by
# `^v[0-9]{4}\.[0-9]{2}\.[0-9]{2}...$`. A mirror tag cannot match that filter,
# and `--latest=false` below keeps it from taking the badge Homebrew's users
# read. It does occupy one of those 50 rows, so mirror tags stay rare: one per
# ngrok bump, which is a code change anyway.

set -euo pipefail

CHANNEL="https://bin.equinox.io/c/bNyj1mQVY4c"
PLATFORMS=(
  "darwin-arm64 zip"
  "darwin-amd64 zip"
  "linux-arm64 tgz"
  "linux-amd64 tgz"
)

dry_run=false
force=false
version=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) dry_run=true; shift ;;
    --force) force=true; shift ;;
    -*) echo "unknown option $1" >&2; exit 1 ;;
    *)
      [[ -z "$version" ]] || { echo "give one version, not two" >&2; exit 1; }
      version="${1#v}"
      shift
      ;;
  esac
done

if [[ -z "$version" ]]; then
  cat >&2 <<'USAGE'
usage: packaging/ngrok/mirror.sh <version> [--dry-run] [--force]

Name the ngrok version you intend to mirror — for example 3.30.0. Equinox serves
one floating build per platform and the version in its URL is decorative, so this
script cannot ask which version it is about to download; it downloads what is
being served and refuses to publish it under a name it does not report.

Read the version off the channel first if you do not know it:

  curl -fsSL https://bin.equinox.io/c/bNyj1mQVY4c/ngrok-v3-stable-linux-amd64.tgz \
    | tar xz -O ngrok > /tmp/ngrok && chmod +x /tmp/ngrok && /tmp/ngrok version
USAGE
  exit 1
fi

command -v gh >/dev/null || { echo "gh is required to upload the release" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for entry in "${PLATFORMS[@]}"; do
  read -r platform extension <<<"$entry"
  echo "downloading ngrok $platform…" >&2
  curl -fsSL -o "$work/$platform.$extension" \
    "$CHANNEL/ngrok-v3-stable-$platform.$extension"
done

# Confirm the build for the host we are on — the only one we can execute — is
# the version being mirrored. The four are published together from one release,
# so one answer covers them; and if it ever does not, the digests will disagree
# with whatever a laptop downloads and `install` will refuse it rather than run
# it. This is a gate, not a discovery: `version` is what the caller asked for,
# and a channel that has already moved on fails here instead of publishing
# somebody else's bytes under the requested name.
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
reported="$("$work/probe/ngrok" version | awk '{print $NF}')"
[[ -n "$reported" ]] || { echo "could not read a version out of the ngrok binary" >&2; exit 1; }
echo "the $host build reports version $reported" >&2
if [[ "$reported" != "$version" ]]; then
  echo "the channel is serving ngrok $reported, not $version — refusing to mirror it" >&2
  echo "re-run with $reported if that is the version you meant to publish." >&2
  exit 1
fi

tag="ngrok-v$version"
echo
echo "  pub const NGROK_VERSION: &str = \"$version\";"
echo "  const NGROK_MIRROR: &str = \"https://github.com/Clubria/riabuild/releases/download/$tag\";"
echo

# Collected once and printed twice: to the terminal for the person pasting them
# into `tools.rs`, and into the release notes so anyone auditing the mirror can
# check it against Equinox without cloning anything.
notes="$work/notes.md"
{
  echo "ngrok $version, republished **unmodified** from <$CHANNEL> so riabuild can pin it"
  echo "and verify it against a digest committed to its own repository. **This is not a"
  echo "riabuild release** — see the \`v<date>\` tags for those."
  echo
  echo "Why this exists: Equinox serves one floating build per platform, and the version in"
  echo "that URL is decorative — a URL naming a version nobody published returns the current"
  echo "bytes all the same. There is no immutable URL to pin and no published checksum file,"
  echo "so riabuild takes a copy of the bytes it verified rather than downloading something"
  echo "unverified, or letting a server tell a laptop which bytes to trust."
  echo
  echo "| asset | sha256 |"
  echo "|---|---|"
} > "$notes"

declare -A digests=()
for entry in "${PLATFORMS[@]}"; do
  read -r platform extension <<<"$entry"
  asset="ngrok-$version-$platform.$extension"
  mv "$work/$platform.$extension" "$work/$asset"
  digest="$(shasum -a 256 "$work/$asset" | awk '{print $1}')"
  digests["$asset"]="$digest"
  printf '  %-34s %s\n' "$platform" "$digest"
  printf '| `%s` | `%s` |\n' "$asset" "$digest" >> "$notes"
done

{
  echo
  echo "The same digests are constants in \`riabuild-cli/crates/fetch/src/tools.rs\`, and"
  echo "\`tools::install\` refuses anything that does not match. Regenerate all of this with"
  echo "\`./packaging/ngrok/mirror.sh $version\`."
} >> "$notes"
echo

# Compared against what is already published *before* anything is uploaded, and
# on --dry-run too: the question a re-run most needs answered is whether it would
# replace bytes somebody is already pinning.
#
# This is where a re-run used to do real damage. Equinox serves floating builds,
# so a second run for the same version downloads whatever is being served *now* —
# and `--clobber` would replace the published bytes under a tag `tools.rs` pins
# with `Checksum::Pinned`. Every laptop would then fetch an asset whose digest no
# longer matches the constant compiled into the binary it is running, and
# `tools::install` would refuse it. The whole fleet loses ngrok, and nothing in
# this repository changed.
#
# Identical bytes are a no-op worth allowing: a run interrupted after two of four
# uploads has to be finishable. Different bytes are a different build wearing an
# old name, and the answer to that is a new tag, not --force. --force is for the
# one case that is neither — an asset uploaded corrupt, being replaced by what it
# should have been all along.
#
# GitHub reports each asset's sha256 in the API, so this costs one request rather
# than re-downloading the release.
release_exists=false
if gh release view "$tag" >/dev/null 2>&1; then
  release_exists=true
  existing="$(gh api "repos/{owner}/{repo}/releases/tags/$tag" \
    --jq '.assets[] | "\(.name) \(.digest // "unknown")"')"

  conflict=false
  while read -r name remote_digest; do
    [[ -n "$name" ]] || continue
    local_digest="${digests[$name]:-}"
    # An asset this run is not producing is none of its business.
    [[ -n "$local_digest" ]] || continue
    [[ "$remote_digest" == "sha256:$local_digest" ]] && continue

    if [[ "$remote_digest" == "unknown" ]]; then
      echo "  $name: GitHub reports no digest, so it cannot be shown to match" >&2
    else
      echo "  $name" >&2
      echo "    published:  ${remote_digest#sha256:}" >&2
      echo "    downloaded: $local_digest" >&2
    fi
    conflict=true
  done <<<"$existing"

  if $conflict; then
    echo >&2
    echo "$tag already holds bytes this run cannot show to be the same." >&2
    if ! $force; then
      echo "Refusing to overwrite them: a laptop pinning $tag would stop being able" >&2
      echo "to install ngrok at all, because tools.rs verifies the digest of what it" >&2
      echo "downloads against a constant it was compiled with. Publish these bytes" >&2
      echo "under the version they actually are, or re-run with --force if you are" >&2
      echo "deliberately replacing a bad upload." >&2
      exit 1
    fi
    echo "--force given: replacing them." >&2
  fi
fi

if $dry_run; then
  echo "--dry-run: nothing uploaded." >&2
  exit 0
fi

if $release_exists; then
  # A re-run after a partial upload should not leave last run's digests standing.
  gh release edit "$tag" --notes-file "$notes"
else
  gh release create "$tag" \
    --title "ngrok $version (mirrored)" \
    --notes-file "$notes" \
    --latest=false
fi
gh release upload "$tag" "$work"/ngrok-"$version"-*.{zip,tgz} --clobber

echo "uploaded to $tag — now update tools.rs with the constants above." >&2
