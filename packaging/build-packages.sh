#!/usr/bin/env bash
#
# Renders packaging/debian/control.in and packaging/rpm/riabuild.spec.in around a
# riabuild binary and builds the .deb and .rpm the release ships.
#
# This exists because there were two of it. `.github/workflows/ci.yml` built
# packages from a stub binary to prove the templates render, and
# `.github/workflows/release.yml` built them from the real one — a hundred lines
# of near-identical shell in two files, which had already drifted: only the
# release copy copied the licence into the deb, and only the CI copy asserted the
# rpm still declares `Requires: git`. Each missed what the other caught, and
# neither miss was visible from the file it was missing in. One script, called
# from both, is what keeps them honest.
#
#   packaging/build-packages.sh --version 2026.08.21 --binary path/to/riabuild \
#     --deb-arch amd64 --rpm-arch x86_64
#
# Either package can be built alone: pass only --deb-arch or only --rpm-arch.
# CI needs that, because `rpmbuild --target aarch64` on an x86_64 host fails with
# "No compatible architectures found for build" while dpkg-deb happily builds
# both architectures anywhere.
#
# Everything lands in --outdir (default ./dist), named exactly as the release
# names it, because Formula/riabuild.rb, the apt pool and the container install
# tests all address these files by name.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

version=""
binary=""
deb_arch=""
rpm_arch=""
outdir="$PWD/dist"
builddir="$PWD/build"

die() { echo "build-packages.sh: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) version="${2:?--version needs a value}"; shift 2 ;;
    --binary) binary="${2:?--binary needs a value}"; shift 2 ;;
    --deb-arch) deb_arch="${2:?--deb-arch needs a value}"; shift 2 ;;
    --rpm-arch) rpm_arch="${2:?--rpm-arch needs a value}"; shift 2 ;;
    --outdir) outdir="${2:?--outdir needs a value}"; shift 2 ;;
    --builddir) builddir="${2:?--builddir needs a value}"; shift 2 ;;
    *) die "unknown argument $1" ;;
  esac
done

[ -n "$version" ] || die "--version is required"
[ -n "$binary" ] || die "--binary is required"
[ -f "$binary" ] || die "--binary $binary does not exist"
[ -n "$deb_arch" ] || [ -n "$rpm_arch" ] || die "pass --deb-arch, --rpm-arch, or both"

# Absolute from here on: rpmbuild's --define paths must be absolute, and the
# callers run from two different working directories.
binary="$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")"
mkdir -p "$outdir" "$builddir"
outdir="$(cd "$outdir" && pwd)"
builddir="$(cd "$builddir" && pwd)"

# A placeholder the release workflow does not substitute is the mistake that
# actually happens here, and it produces a package that installs a version
# literally called `@VERSION@`.
check_rendered() {
  local rendered="$1"
  if grep -q '@[A-Z0-9_]*@' "$rendered"; then
    echo "::error::$rendered still contains unsubstituted placeholders:" >&2
    grep -n '@[A-Z0-9_]*@' "$rendered" >&2
    exit 1
  fi
}

# The shell completion scripts both packages install, generated once by the
# binary being packaged.
#
# Generated rather than kept as three files in packaging/: they are rendered
# from the same clap `Cli` the binary parses argv with, so a renamed subcommand
# cannot leave a stale completion behind. See `internal::completions`.
#
# **This runs $binary**, which is fine in both callers and worth saying out loud
# because it is the one assumption here that a future change could break.
# release.yml packages each architecture on the runner that built it — its own
# comment gives rpmbuild's refusal to cross-build as the reason — and ci.yml
# passes a stub shell script. A caller that packaged a foreign-architecture
# binary would get "cannot execute binary file" here. Completion scripts carry
# nothing architecture-specific, so the fix in that case is to generate once and
# reuse the output, never to skip them.
#
# Written to a file rather than piped, so a short write is an error rather than
# a truncated completion nobody notices. Emptiness is checked for the same
# reason: `set -o pipefail` cannot see a binary that exits 0 having printed
# nothing, and an empty file installed where a shell autoloads it is a broken
# completion rather than an absent one.
completions="$builddir/completions"
rm -rf "$completions"
mkdir -p "$completions"
for shell in bash zsh fish; do
  case "$shell" in
    # The names each shell expects. bash and fish look for the command's own
    # name; zsh's autoloader wants the `_name` convention.
    bash) out="$completions/riabuild" ;;
    zsh) out="$completions/_riabuild" ;;
    fish) out="$completions/riabuild.fish" ;;
  esac
  "$binary" internal completions "$shell" > "$out"
  [ -s "$out" ] || die "$binary printed no $shell completion script"
done

if [ -n "$deb_arch" ]; then
  debroot="$builddir/deb-$deb_arch"
  rm -rf "$debroot"
  mkdir -p "$debroot/DEBIAN" "$debroot/usr/bin" "$debroot/usr/share/doc/riabuild"
  install -m0755 "$binary" "$debroot/usr/bin/riabuild"
  cp "$root/README.md" "$debroot/usr/share/doc/riabuild/README.md"
  # Debian looks for the licence under this name, and MIT asks that the notice
  # travel with every copy — including a binary one.
  cp "$root/LICENSE" "$debroot/usr/share/doc/riabuild/copyright"
  # The three directories Debian's own shells already search, so a developer who
  # installs the package has completions on their next shell and does nothing.
  # `vendor-completions` rather than `site-functions` for zsh: the former is
  # what a package owns and the latter is the local administrator's.
  install -Dm0644 "$completions/riabuild" \
    "$debroot/usr/share/bash-completion/completions/riabuild"
  install -Dm0644 "$completions/_riabuild" \
    "$debroot/usr/share/zsh/vendor-completions/_riabuild"
  install -Dm0644 "$completions/riabuild.fish" \
    "$debroot/usr/share/fish/vendor_completions.d/riabuild.fish"
  sed -e "s|@VERSION@|$version|g" -e "s|@DEB_ARCH@|$deb_arch|g" \
    "$root/packaging/debian/control.in" > "$debroot/DEBIAN/control"
  check_rendered "$debroot/DEBIAN/control"
  # --root-owner-group so the package does not carry the builder's uid, which
  # would install files owned by a user that does not exist.
  dpkg-deb --build --root-owner-group "$debroot" \
    "$outdir/riabuild_${version}_${deb_arch}.deb"
fi

if [ -n "$rpm_arch" ]; then
  rpmsrc="$builddir/src-$rpm_arch"
  rm -rf "$rpmsrc"
  mkdir -p "$rpmsrc"
  install -m0755 "$binary" "$rpmsrc/riabuild"
  cp "$root/LICENSE" "$rpmsrc/LICENSE"
  # There is no %prep, so everything the spec installs has to be in _sourcedir —
  # the same route LICENSE above already takes.
  cp "$completions/riabuild" "$rpmsrc/riabuild.bash"
  cp "$completions/_riabuild" "$rpmsrc/_riabuild"
  cp "$completions/riabuild.fish" "$rpmsrc/riabuild.fish"
  spec="$builddir/riabuild-$rpm_arch.spec"
  sed -e "s|@VERSION@|$version|g" -e "s|@RPM_ARCH@|$rpm_arch|g" \
    "$root/packaging/rpm/riabuild.spec.in" > "$spec"
  check_rendered "$spec"
  rpmbuild -bb "$spec" \
    --define "_sourcedir $rpmsrc" \
    --define "_rpmdir $outdir" \
    --define "_topdir $builddir/rpm-$rpm_arch"
  # rpmbuild nests its output under the architecture; flatten it so no caller
  # has to know that.
  mv "$outdir/$rpm_arch"/*.rpm "$outdir/"
  rmdir "$outdir/$rpm_arch"
fi

ls -l "$outdir/"
