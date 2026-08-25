import { describe, expect, test } from "vitest";
import { INSTALL_CHOICES, guessPlatform } from "./platform";

// Real user agent strings, because the guess is a substring match and the
// interesting cases are the ones where two substrings both appear.
const AGENTS = {
  macChrome:
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36",
  macSafari:
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15",
  ubuntuFirefox:
    "Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0",
  fedoraChrome:
    "Mozilla/5.0 (X11; Fedora; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36",
  android:
    "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Mobile Safari/537.36",
  windows:
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36",
};

describe("guessPlatform", () => {
  test("a Mac gets Homebrew", () => {
    expect(guessPlatform(AGENTS.macChrome)).toBe("macos");
    expect(guessPlatform(AGENTS.macSafari)).toBe("macos");
  });

  test("a Linux desktop gets apt", () => {
    // Both of these are X11 + Linux. Fedora would rather have dnf, and clicks
    // once — apt covers Ubuntu, Debian and Mint, which is most of the field.
    expect(guessPlatform(AGENTS.ubuntuFirefox)).toBe("apt");
    expect(guessPlatform(AGENTS.fedoraChrome)).toBe("apt");
  });

  test("Android is not a Linux desktop", () => {
    // Android's UA contains "Linux", so a naive check offers apt on a phone.
    expect(guessPlatform(AGENTS.android)).toBe("macos");
  });

  test("anything unrecognised falls back rather than throwing", () => {
    // Windows is not a platform riabuild ships to, and an empty string is what
    // server-side rendering would pass. Neither may produce `undefined` — the
    // component indexes the choice list with whatever comes back.
    expect(guessPlatform(AGENTS.windows)).toBe("macos");
    expect(guessPlatform("")).toBe("macos");
  });

  test("every guess names a choice that exists", () => {
    for (const agent of Object.values(AGENTS)) {
      const guess = guessPlatform(agent);
      expect(INSTALL_CHOICES.map((choice) => choice.id)).toContain(guess);
    }
  });
});

describe("install commands", () => {
  test("each one installs riabuild", () => {
    for (const choice of INSTALL_CHOICES) {
      expect(choice.command).toContain("riabuild");
    }
  });

  test("apt and dnf point at the published repository", () => {
    // The URL is the whole instruction. A typo here is a developer following
    // three lines of shell to a 404.
    const apt = INSTALL_CHOICES.find((c) => c.id === "apt")!;
    const dnf = INSTALL_CHOICES.find((c) => c.id === "dnf")!;

    expect(apt.command).toContain(
      "https://clubria.github.io/riabuild/clubria.gpg",
    );
    expect(apt.command).toContain("https://clubria.github.io/riabuild/deb");
    expect(apt.command).toContain("signed-by=/usr/share/keyrings/clubria.gpg");
    // Never a hardcoded architecture: an arm64 machine handed arch=amd64 gets a
    // repository apt reads happily and finds nothing in.
    expect(apt.command).toContain("$(dpkg --print-architecture)");
    expect(apt.command).not.toContain("arch=amd64");

    expect(dnf.command).toContain(
      "https://clubria.github.io/riabuild/rpm/clubria.repo",
    );
    expect(dnf.command).toContain("/etc/yum.repos.d/");
  });

  test("Homebrew installs in one line, with no explicit tap", () => {
    // `brew install clubria/tap/riabuild` auto-taps Clubria/homebrew-tap, which
    // is a real repository carrying the formula. A `brew tap` line creeping
    // back in is the regression worth catching: it would pin developers to a
    // tap remote pointing at Clubria/riabuild, which is the copy that gets
    // retired first.
    const brew = INSTALL_CHOICES.find((c) => c.id === "macos")!;
    expect(brew.command).toBe("brew install clubria/tap/riabuild");
    expect(brew.command).not.toContain("brew tap");
  });
});
