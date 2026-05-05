class Ainb < Formula
  desc "Terminal-based development environment manager for Claude Code agents"
  homepage "https://github.com/stevengonsalvez/agents-in-a-box"
  license "MIT"
  version "1.0.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/stevengonsalvez/agents-in-a-box/releases/download/v1.0.0/ainb-1.0.0-aarch64-apple-darwin.tar.gz"
      sha256 "f920c5bb0748cf4d71ee08f7828bc7ba039bcbde9a94d5f156d3f589617589a6"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/stevengonsalvez/agents-in-a-box/releases/download/v1.0.0/ainb-1.0.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e41fc2aeadc7c72457422e2b2297b16c4873e81cf2487cbe8291957db48d0a7c"
    end
  end

  def install
    bin.install "ainb"
  end

  test do
    assert_match "ainb", shell_output("#{bin}/ainb --version")
  end
end
