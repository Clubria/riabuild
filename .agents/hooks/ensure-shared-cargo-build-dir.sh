#!/usr/bin/env bash
#
# Creates the machine-local cargo config that shares one compiled dependency
# graph across every checkout and worktree while leaving each of them its own
# finished binaries. See riabuild-cli/AGENTS.md for why the file is untracked
# and why it must live at the main checkout root.
#
# Run from a SessionStart hook, so a fresh clone is configured before anyone
# builds twice. Writing the config is the whole job — worktrees need no
# per-worktree step, because cargo walks up from the worktree and finds this
# one file on its way to the filesystem root.
#
# This must never fail a session. Every path exits 0.

set -uo pipefail

# --git-common-dir resolves to the MAIN checkout's .git even when this runs
# inside a worktree, which is exactly the anchor needed: the config only shares
# anything if it lands at the main checkout root rather than in a worktree.
common_dir=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || exit 0
[ -n "$common_dir" ] || exit 0

repo_root=$(dirname "$common_dir")
[ -d "$repo_root" ] || exit 0

config="$repo_root/.cargo/config.toml"

# Comments and whitespace stripped and the keys sorted, so this recognises a
# config the hook wrote without depending on the exact prose above it — which
# has changed before and will change again.
normalise() {
	sed -e 's/#.*$//' -e 's/[[:space:]]//g' "$1" 2>/dev/null |
		grep -v '^$' | sort | tr '\n' ';'
}

current='[build];build-dir="shared-build";'
# What this hook wrote before the split: one target/ shared by everything,
# finished binaries included. That is the configuration this replaces.
superseded='[build];target-dir="target";'

write_config() {
	mkdir -p "$repo_root/.cargo" 2>/dev/null || return 1
	cat >"$config" <<'TOML' 2>/dev/null || return 1
# Machine-local, deliberately untracked. Shares one compiled dependency graph
# across the main checkout and every worktree under .claude/worktrees/, while
# leaving each of them its own finished binaries. Rationale, and the reason
# this must not be committed, are in riabuild-cli/AGENTS.md.
#
# build-dir   resolved against the directory holding this .cargo, so every
#             checkout and worktree shares <repo>/shared-build for deps/,
#             .fingerprint/, build/ and incremental/ — and serialises on the
#             one .cargo-build-lock inside it.
#
# target-dir  deliberately unset. It then defaults to <workspace-root>/target,
#             which is per-worktree, so riabuild-cli/target/debug/riabuild is
#             the binary this worktree built and not whichever one finished
#             last. Setting it here is what used to make them collide.
#
# Written by .agents/hooks/ensure-shared-cargo-build-dir.sh. Delete it to opt
# out for this machine; it will be recreated on the next session start.
[build]
build-dir = "shared-build"
TOML
}

if [ -e "$config" ]; then
	found=$(normalise "$config")

	# Already current: say nothing. This is the common case, every session.
	[ "$found" = "$current" ] && exit 0

	if [ "$found" != "$superseded" ]; then
		# Someone customised it — pointed the build at another disk, most
		# likely. Never clobber that. Worth one line if the customisation is
		# the specific one that reintroduces the bug this hook exists to fix.
		case "$found" in
		*target-dir*)
			echo "riabuild: $config sets target-dir, which makes every worktree" >&2
			echo "riabuild: write one shared riabuild-cli/target/debug/riabuild." >&2
			echo "riabuild: Prefer build-dir — see riabuild-cli/AGENTS.md." >&2
			;;
		esac
		exit 0
	fi

	write_config || exit 0

	echo "riabuild: cargo config upgraded — dependencies stay shared, but each" >&2
	echo "riabuild: worktree now builds its own riabuild-cli/target/debug/riabuild." >&2
	if [ -d "$repo_root/target" ] && [ ! -e "$repo_root/shared-build" ]; then
		echo "riabuild: $repo_root/target is now unused. Keep its warm cache with" >&2
		echo "riabuild:   mv '$repo_root/target' '$repo_root/shared-build'" >&2
		echo "riabuild: or reclaim the space with" >&2
		echo "riabuild:   rm -rf '$repo_root/target'" >&2
	fi
	exit 0
fi

write_config || exit 0

echo "riabuild: shared cargo build-dir configured at $config" >&2
exit 0
