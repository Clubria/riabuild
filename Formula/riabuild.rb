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
  version "2026.08.18"
  license "MIT"

  # Homebrew runs on Linux and this formula would install there. It should not:
  # Linux is served by the apt and dnf repositories, and `update.rs` upgrades
  # riabuild through whichever package manager owns the running binary. Homebrew
  # on Linux owns it in a way neither `dpkg -S` nor `rpm -qf` can see, so a
  # brew-installed Linux riabuild would quietly never update itself again.
  #
  # Refusing here makes that a clear message at install time rather than a
  # machine that silently stops keeping up.
  depends_on :macos

  on_arm do
    url "https://github.com/Clubria/riabuild/releases/download/v2026.08.18/riabuild-2026.08.18-aarch64-apple-darwin.tar.gz"
    sha256 "af73e064e3a3b152e29150feb0b1ddd661308035d8dab84a0e8668e77ab07be0"
  end

  on_intel do
    url "https://github.com/Clubria/riabuild/releases/download/v2026.08.18/riabuild-2026.08.18-x86_64-apple-darwin.tar.gz"
    sha256 "ac8a182cfa5d9ecca12df2707a09c59864616185bd469fc8559de1391dd2f662"
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
    assert_match "riabuild 2026.08.18", shell_output("#{bin}/riabuild --version")
  end
end
