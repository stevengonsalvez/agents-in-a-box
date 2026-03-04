class Ainb < Formula
  desc "Terminal-based development environment manager for Claude Code agents"
  homepage "https://github.com/stevengonsalvez/agents-in-a-box"
  license "MIT"
  version "0.5.3-beta1"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/stevengonsalvez/agents-in-a-box/releases/download/v0.5.3-beta1/agents-box-0.5.3-beta1-aarch64-apple-darwin.tar.gz"
      sha256 "c68a6155796e47267515f41b4faa4394d6bf94afb02e0566986c137f381e3aee"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/stevengonsalvez/agents-in-a-box/releases/download/v0.5.3-beta1/agents-box-0.5.3-beta1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "2a24201df60cf691b1b802b03fc522fbd748374f4008b84e4ec4ff49ff6589a4"
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
