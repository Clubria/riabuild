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
  version "@VERSION@"

  # v1 targets macOS. paths.rs and keychain.rs are trait-shaped so Linux is an
  # addition rather than a rewrite, but no Linux build is published yet, and a
  # formula that installs a binary with a stub keychain would fail confusingly
  # at the first `riabuild login` instead of here.
  depends_on :macos

  on_arm do
    url "https://github.com/Clubria/riabuild/releases/download/v@VERSION@/riabuild-@VERSION@-aarch64-apple-darwin.tar.gz"
    sha256 "@SHA256_AARCH64@"
  end

  on_intel do
    url "https://github.com/Clubria/riabuild/releases/download/v@VERSION@/riabuild-@VERSION@-x86_64-apple-darwin.tar.gz"
    sha256 "@SHA256_X86_64@"
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
    assert_match "riabuild #{version}", shell_output("#{bin}/riabuild --version")
  end
end
