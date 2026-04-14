class Ainb < Formula
  desc "Terminal-based development environment manager for Claude Code agents"
  homepage "https://github.com/stevengonsalvez/agents-in-a-box"
  license "MIT"
  version "0.5.5-beta1"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/stevengonsalvez/agents-in-a-box/releases/download/v0.5.5-beta1/agents-box-0.5.5-beta1-aarch64-apple-darwin.tar.gz"
      sha256 "7144812eda3846a12f6974d5c284cb54bf4fd36f8ba6c51c20a47d20afa06880"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/stevengonsalvez/agents-in-a-box/releases/download/v0.5.5-beta1/agents-box-0.5.5-beta1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "621a488ef1c3e7b7bba156d099c9113742fa6c06885be897834de6ecf3bf093a"
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
