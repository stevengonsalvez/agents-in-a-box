class Ainb < Formula
  desc "Terminal-based development environment manager for Claude Code agents"
  homepage "https://github.com/stevengonsalvez/agents-in-a-box"
  license "MIT"
  version "0.5.4-beta1"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/stevengonsalvez/agents-in-a-box/releases/download/v0.5.4-beta1/agents-box-0.5.4-beta1-aarch64-apple-darwin.tar.gz"
      sha256 "30abbfdf39309e91a0114ad6582eba22c7146748e195b94833fec1e779071d97"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/stevengonsalvez/agents-in-a-box/releases/download/v0.5.4-beta1/agents-box-0.5.4-beta1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "19a3dfa899396436a97d311255ecc2c3bd2b410cbc09fa5460ff9f152cbb5c86"
    end
  end

  def install
    bin.install "agents-box"
    bin.install_symlink "agents-box" => "ainb"
  end

  test do
    assert_match "agents-box", shell_output("#{bin}/agents-box --version")
  end
end
