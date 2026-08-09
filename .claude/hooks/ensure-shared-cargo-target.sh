#!/usr/bin/env bash
#
# Creates the machine-local cargo config that points every checkout and worktree
# at one shared target/ directory. See riabuild-cli/CLAUDE.md for why that file
# is untracked and why it must live at the main checkout root.
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

# Idempotent: never clobber a config someone has customised, e.g. pointed at a
# different disk.
[ -e "$config" ] && exit 0

mkdir -p "$repo_root/.cargo" 2>/dev/null || exit 0

cat > "$config" <<'TOML' 2>/dev/null || exit 0
# Machine-local, deliberately untracked. Points the main checkout and every
# worktree under .claude/worktrees/ at one shared target/ at the repository
# root. Rationale, and the reason this must not be committed, are in
# riabuild-cli/CLAUDE.md.
#
# Written by .claude/hooks/ensure-shared-cargo-target.sh. Delete it to opt out
# for this machine; it will be recreated on the next session start.
[build]
target-dir = "target"
TOML

echo "riabuild: shared cargo target/ configured at $config" >&2
exit 0
