/**
 * Which package manager the install instructions should open on.
 *
 * Split out of the component so it can be tested without rendering anything,
 * and because a module that exports both a component and a helper breaks fast
 * refresh.
 */

export type Platform = "macos" | "apt" | "dnf";

const REPO = "https://clubria.github.io/riabuild";

export type InstallChoice = {
  id: Platform;
  label: string;
  /** Who this is for, in the words they would use for themselves. */
  audience: string;
  command: string;
};

export const INSTALL_CHOICES: InstallChoice[] = [
  {
    id: "macos",
    label: "homebrew",
    audience: "macOS, Apple silicon or Intel.",
    // The explicit `brew tap` line is part of the install, not decoration:
    // Homebrew auto-taps `clubria/tap` only when it can derive the repository
    // name, and the name it derives is Clubria/homebrew-tap.
    command: `brew tap clubria/tap https://github.com/Clubria/riabuild
brew install clubria/tap/riabuild`,
  },
  {
    id: "apt",
    label: "apt",
    audience: "Debian, Ubuntu, and derivatives.",
    // `dpkg --print-architecture` rather than a hardcoded amd64: an arm64
    // machine handed `arch=amd64` gets a repository apt reads happily and finds
    // nothing in, which reads as riabuild not being published at all.
    command: `curl -fsSL ${REPO}/clubria.gpg \\
  | sudo tee /usr/share/keyrings/clubria.gpg >/dev/null
echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/clubria.gpg] ${REPO}/deb stable main" \\
  | sudo tee /etc/apt/sources.list.d/clubria.list >/dev/null
sudo apt update && sudo apt install riabuild`,
  },
  {
    id: "dnf",
    label: "dnf",
    audience: "Fedora, RHEL, and derivatives.",
    // Copied into place rather than added with `dnf config-manager`, whose
    // spelling changed between dnf4 (`--add-repo`) and dnf5
    // (`addrepo --from-repofile=`). curl works on both and will keep working.
    //
    // One long line rather than a backslash continuation: at 380px every line
    // soft-wraps anyway, and a continuation there renders as a lone `\` above
    // an apparently blank line. apt below keeps its continuations because it is
    // genuinely several commands.
    command: `sudo curl -fsSL -o /etc/yum.repos.d/clubria.repo ${REPO}/rpm/clubria.repo
sudo dnf install riabuild`,
  },
];

/**
 * What this visitor is probably running.
 *
 * A guess, not a decision. `navigator.userAgent` is advisory at best, and a
 * developer on a Linux laptop reading this on a Mac is an ordinary Tuesday.
 * Getting it wrong costs one click, which is why this never tries harder than
 * a substring — the other two choices are visible beside it, not hidden.
 */
export function guessPlatform(userAgent: string): Platform {
  const agent = userAgent.toLowerCase();
  // Before "linux": Android reports both, and neither apt nor dnf is the
  // answer there. Nobody installs riabuild on a phone, so the first tab should
  // stay the common case rather than becoming the one that fits a substring.
  if (agent.includes("android")) return "macos";
  if (agent.includes("linux") || agent.includes("x11")) {
    // Ubuntu is the overwhelmingly common Linux desktop, and apt covers it
    // along with Debian and Mint. A Fedora developer clicks once.
    return "apt";
  }
  return "macos";
}
