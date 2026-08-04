/**
 * Dotted-numeric version comparison for `minCliVersion` / `latestCliVersion`.
 *
 * Deliberately not full semver: the CLI is versioned by us, ships from one
 * Homebrew tap, and never carries prerelease or build metadata. A prerelease
 * suffix is ignored rather than rejected so a malformed version can never wedge
 * a developer out of their environment.
 */
export function parseVersion(version: string): number[] {
  const core = version.trim().replace(/^v/, "").split(/[-+]/)[0];
  return core
    .split(".")
    .map((part) => Number.parseInt(part, 10))
    .map((part) => (Number.isFinite(part) ? part : 0));
}

/** -1 if a < b, 0 if equal, 1 if a > b. */
export function compareVersions(a: string, b: string): number {
  const left = parseVersion(a);
  const right = parseVersion(b);
  const length = Math.max(left.length, right.length);
  for (let i = 0; i < length; i++) {
    const l = left[i] ?? 0;
    const r = right[i] ?? 0;
    if (l !== r) return l < r ? -1 : 1;
  }
  return 0;
}

export function meetsMinimum(version: string, minimum: string): boolean {
  return compareVersions(version, minimum) >= 0;
}
