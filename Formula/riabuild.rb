# Template for the formula this repository serves as a Homebrew tap.
#
# `.github/workflows/release.yml` substitutes the at-sign-delimited placeholders
# below and commits the result to `Formula/riabuild.rb` on main, which is where
# `brew tap` looks. Edit this file, never `Formula/riabuild.rb` — the next
# release overwrites that one.
#
# The workflow rejects a rendered formula that still contains a placeholder, so
# this comment deliberately describes their shape rather than spelling one out.
class Riabuild < Formula
  desc "Sets up a Clubria developer's machine and opens the Clubria environment"
  homepage "https://riabuild.clubria.com"
  version "2026.08.04"

  # v1 targets macOS. paths.rs and keychain.rs are trait-shaped so Linux is an
  # addition rather than a rewrite, but no Linux build is published yet, and a
  # formula that installs a binary with a stub keychain would fail confusingly
  # at the first `riabuild login` instead of here.
  depends_on :macos

  on_arm do
    url "https://github.com/Clubria/riabuild/releases/download/v2026.08.04/riabuild-2026.08.04-aarch64-apple-darwin.tar.gz"
    sha256 "b4ec804b23001fc5ef3cd2cc33d1c94141392ea1371602bf0a7549d7f7c1f8ef"
  end

  on_intel do
    url "https://github.com/Clubria/riabuild/releases/download/v2026.08.04/riabuild-2026.08.04-x86_64-apple-darwin.tar.gz"
    sha256 "4c85ae4a743bed4d35d5e73be89d76bf386f63d0b487ab759d0c785eba11dffe"
  end

  def install
    bin.install "riabuild"
  end

  def caveats
    <<~EOS
      Run `riabuild` to set this machine up and open the Clubria environment.

      riabuild keeps its state in ~/.riabuild and your session token in the
      login keychain. `brew uninstall riabuild` leaves both in place; to remove
      them as well:

        riabuild logout && rm -rf ~/.riabuild
    EOS
  end

  test do
    # Compared against the literal rather than #{version}: riabuild's versions
    # are zero-padded release dates, and this asserts what the binary prints
    # without depending on how Homebrew's own Version renders "2026.08.04".
    assert_match "riabuild 2026.08.04", shell_output("#{bin}/riabuild --version")
  end
end
