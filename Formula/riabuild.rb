# Template for the formula riabuild is installed by on macOS.
#
# `.github/workflows/release.yml` substitutes the at-sign-delimited placeholders
# below and commits the result to `Formula/riabuild.rb` in two repositories:
# `Clubria/homebrew-tap`, which is the one `brew install clubria/tap/riabuild`
# reaches for on its own, and this one, which is where the laptops that ran the
# old explicit `brew tap` against this repository still upgrade from.
#
# Edit this file, never either rendered `Formula/riabuild.rb` — the next release
# overwrites both.
#
# The workflow rejects a rendered formula that still contains a placeholder, so
# this comment deliberately describes their shape rather than spelling one out.
class Riabuild < Formula
  desc "Sets up a Clubria developer's machine and opens the Clubria environment"
  homepage "https://riabuild.clubria.com"
  version "2026.08.27"
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
    url "https://github.com/Clubria/riabuild/releases/download/v2026.08.27/riabuild-2026.08.27-aarch64-apple-darwin.tar.gz"
    sha256 "3e26445f6e419e97e1ce4545b4a653269b3bd721d542e7b8adecaa55720e5f1e"
  end

  on_intel do
    url "https://github.com/Clubria/riabuild/releases/download/v2026.08.27/riabuild-2026.08.27-x86_64-apple-darwin.tar.gz"
    sha256 "6df123301a6ab647a535c843e4f6b590edd41d7d7dc7cd9b42bd46b1d521f45d"
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
    assert_match "riabuild 2026.08.27", shell_output("#{bin}/riabuild --version")
  end
end
