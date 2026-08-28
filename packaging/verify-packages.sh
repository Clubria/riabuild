#!/usr/bin/env bash
#
# Asserts that a built .deb and .rpm describe what they contain, and — with
# --containers — that a real apt and a real dnf install them.
#
# The other half of packaging/build-packages.sh, and here for the same reason:
# ci.yml and release.yml each carried their own copy of these assertions and the
# copies had diverged. The set below is the *union* of what the two checked, so
# every caller now catches everything either one used to.
#
#   packaging/verify-packages.sh --version 2026.08.21 \
#     --deb dist/riabuild_2026.08.21_amd64.deb --deb-arch amd64 \
#     --rpm dist/riabuild-2026.08.21-1.x86_64.rpm --rpm-arch x86_64 \
#     --containers
#
# Either package can be verified alone, matching build-packages.sh. --containers
# is separate because a caller can only run a container for its own
# architecture: CI builds an arm64 deb on an x86_64 runner and can check its
# metadata but not install it.
#
# The container images are pinned by digest here rather than in each workflow.
# A mutable tag is a supply-chain hole in a gate like this one — the whole point
# of the install test is that a *specific* apt and a *specific* dnf accept the
# package, and `debian:12` is whatever was pushed under that name this morning.

set -euo pipefail

version=""
deb=""
rpm=""
deb_arch=""
rpm_arch=""
containers=false

# debian:12 and fedora:41. Both are multi-architecture indexes, so one digest
# resolves correctly on an x86_64 and an arm64 runner alike.
DEB_IMAGE="debian:12@sha256:813017f3d62be4b5891a7acca6a01bdcd4b8513daa81b1ab99d3a50385b26931"
RPM_IMAGE="fedora:41@sha256:f1a3fab47bcb3c3ddf3135d5ee7ba8b7b25f2e809a47440936212a3a50957f3d"

die() { echo "verify-packages.sh: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) version="${2:?--version needs a value}"; shift 2 ;;
    --deb) deb="${2:?--deb needs a value}"; shift 2 ;;
    --rpm) rpm="${2:?--rpm needs a value}"; shift 2 ;;
    --deb-arch) deb_arch="${2:?--deb-arch needs a value}"; shift 2 ;;
    --rpm-arch) rpm_arch="${2:?--rpm-arch needs a value}"; shift 2 ;;
    --deb-image) DEB_IMAGE="${2:?--deb-image needs a value}"; shift 2 ;;
    --rpm-image) RPM_IMAGE="${2:?--rpm-image needs a value}"; shift 2 ;;
    --containers) containers=true; shift ;;
    *) die "unknown argument $1" ;;
  esac
done

[ -n "$version" ] || die "--version is required"
[ -n "$deb" ] || [ -n "$rpm" ] || die "pass --deb, --rpm, or both"
[ -z "$deb" ] || [ -n "$deb_arch" ] || die "--deb needs --deb-arch"
[ -z "$rpm" ] || [ -n "$rpm_arch" ] || die "--rpm needs --rpm-arch"
[ -z "$deb" ] || [ -f "$deb" ] || die "$deb does not exist"
[ -z "$rpm" ] || [ -f "$rpm" ] || die "$rpm does not exist"

if [ -n "$deb" ]; then
  dpkg-deb --info "$deb"
  # Listings are captured before being searched, never piped into `grep -q`.
  # grep exits at its first match and closes the pipe; dpkg-deb's tar subprocess
  # then fails its next write, and `pipefail` turns that into a failed step. It
  # is a race, so it stays invisible until the archive is long enough that tar is
  # still writing when grep leaves — adding the copyright file was enough to
  # cross that line, and the assertion that broke was not the one that changed.
  contents="$(dpkg-deb --contents "$deb")"
  # A package that does not put the binary on PATH installs nothing useful, and
  # neither dpkg-deb nor apt would complain.
  grep -q './usr/bin/riabuild' <<<"$contents"
  # MIT asks that the notice travel with every copy, including a binary one.
  # Debian looks for it under this exact name.
  grep -q './usr/share/doc/riabuild/copyright' <<<"$contents"
  # A completion that is not in the directory its shell searches is not a
  # completion. These paths are the whole feature — nothing on a developer's
  # machine sources them by name, and nothing errors when they are absent, so
  # a wrong directory presents as `riabuild <TAB>` quietly doing nothing.
  grep -q './usr/share/bash-completion/completions/riabuild' <<<"$contents"
  grep -q './usr/share/zsh/vendor-completions/_riabuild' <<<"$contents"
  grep -q './usr/share/fish/vendor_completions.d/riabuild.fish' <<<"$contents"
  test "$(dpkg-deb --field "$deb" Version)" = "$version"
  test "$(dpkg-deb --field "$deb" Architecture)" = "$deb_arch"
fi

if [ -n "$rpm" ]; then
  rpm -qip "$rpm"
  rpm_contents="$(rpm -qlp "$rpm")"
  grep -qx '/usr/bin/riabuild' <<<"$rpm_contents"
  # The deb's three, at Fedora's spelling of the zsh one. Same reasoning: an
  # absent completion is silent on the machine that has it.
  grep -qx '/usr/share/bash-completion/completions/riabuild' <<<"$rpm_contents"
  grep -qx '/usr/share/zsh/site-functions/_riabuild' <<<"$rpm_contents"
  grep -qx '/usr/share/fish/vendor_completions.d/riabuild.fish' <<<"$rpm_contents"
  test "$(rpm -qp --queryformat '%{VERSION}' "$rpm")" = "$version"
  test "$(rpm -qp --queryformat '%{ARCH}' "$rpm")" = "$rpm_arch"
  # AutoReq is off, so an explicit Requires is the only way git gets declared —
  # and dropping it is invisible until someone installs on a machine without git
  # and riabuild fails at the first checkout.
  grep -qx 'git' <<<"$(rpm -qp --requires "$rpm")"
  # `-qL` lists only what is marked %license, so this checks the marking rather
  # than that a file called LICENSE got swept into the payload.
  grep -q 'LICENSE' <<<"$(rpm -qLp "$rpm")"
  test "$(rpm -qp --queryformat '%{LICENSE}' "$rpm")" = "MIT"
fi

if ! $containers; then
  echo "packages describe what they contain."
  exit 0
fi

# The rendered templates producing a *file* is not the same as a package manager
# accepting it. A bad Depends line, a malformed description, or a spec that
# installs somewhere unexpected all pass every check above.
#
# `dpkg -S` and `rpm -qf` are asserted because that is how update.rs decides
# which package manager owns the running binary: a package that installs but is
# not attributable would leave riabuild unable to upgrade itself, and nothing
# else in the suite would notice.
#
# Names are computed out here and passed through the environment so the
# container scripts can be single-quoted. A double-quoted one has to escape
# every `$` it wants the container to expand, and the first missed backslash
# silently interpolates on the host instead.
export VERSION="$version"

if [ -n "$deb" ]; then
  dir="$(cd "$(dirname "$deb")" && pwd)"
  DEB="$(basename "$deb")"
  export DEB
  docker run --rm -e VERSION -e DEB -v "$dir:/dist:ro" "$DEB_IMAGE" \
    bash -eux -c '
      apt-get update -qq
      apt-get install -y -qq "/dist/$DEB"
      test "$(riabuild --version)" = "riabuild $VERSION"
      dpkg -S "$(command -v riabuild)" | grep -q riabuild
      # Sourced, not just present. A generated script that landed in the right
      # directory and does not parse is the failure this catches, and it is
      # otherwise invisible: bash-completion swallows the error, so the only
      # symptom is Tab doing nothing on a developer'"'"'s machine.
      source /usr/share/bash-completion/completions/riabuild
      type _riabuild >/dev/null
    '
fi

if [ -n "$rpm" ]; then
  dir="$(cd "$(dirname "$rpm")" && pwd)"
  RPM="$(basename "$rpm")"
  export RPM
  docker run --rm -e VERSION -e RPM -v "$dir:/dist:ro" "$RPM_IMAGE" \
    bash -eux -c '
      dnf install -y "/dist/$RPM"
      test "$(riabuild --version)" = "riabuild $VERSION"
      rpm -qf "$(command -v riabuild)" | grep -q riabuild
      source /usr/share/bash-completion/completions/riabuild
      type _riabuild >/dev/null
    '
fi

echo "apt and dnf both installed riabuild $version."
