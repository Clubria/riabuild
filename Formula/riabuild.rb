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
  version "2026.08.05"

  # v1 targets macOS. paths.rs and keychain.rs are trait-shaped so Linux is an
  # addition rather than a rewrite, but no Linux build is published yet, and a
  # formula that installs a binary with a stub keychain would fail confusingly
  # at the first `riabuild login` instead of here.
  depends_on :macos

  on_arm do
    url "https://github.com/Clubria/riabuild/releases/download/v2026.08.05/riabuild-2026.08.05-aarch64-apple-darwin.tar.gz"
    sha256 "cd6e016bcbbd6bb28d94f2baae4a41b982a4ae58c5ec56d5643a85f4ab972150"
  end

  on_intel do
    url "https://github.com/Clubria/riabuild/releases/download/v2026.08.05/riabuild-2026.08.05-x86_64-apple-darwin.tar.gz"
    sha256 "e11a6a66f27ee1d8b1605f3fd38e12f2f3038a03725d47e9671458d1547514b0"
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
    assert_match "riabuild 2026.08.05", shell_output("#{bin}/riabuild --version")
  end
end
