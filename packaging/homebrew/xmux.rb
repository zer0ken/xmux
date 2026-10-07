class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.17.2"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.2/xmux-v0.17.2-aarch64-apple-darwin.tar.gz"
      sha256 "7b040dc381a11f9d75520c7a6d2f17dc09000f40a1141034423053d27ffdc909"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.2/xmux-v0.17.2-x86_64-apple-darwin.tar.gz"
      sha256 "a3c9e687ddaadf65be9848167391469d6c001bf6bdb057559921fef9f3e5e1f8"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.2/xmux-v0.17.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5baa194fc76fec06a49aa13470f22de6e0a263a7f17c25b881dd0b8f66171a6a"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.2/xmux-v0.17.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5a8f9c2f946dec950a2e86b7f88faee07d0603223161851ad4706c6e64d3a549"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
