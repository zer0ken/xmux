class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.3"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.3/xmux-v0.9.3-aarch64-apple-darwin.tar.gz"
      sha256 "fe1f86d936d1d6c940da8caac724bff171f4d337b4aae40023742a83c05f4f28"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.3/xmux-v0.9.3-x86_64-apple-darwin.tar.gz"
      sha256 "9f9ccaa7f2339e6e7ff590ce7cfa5acac28408ab4cd5979df3c8d468238b8565"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.3/xmux-v0.9.3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "8abb8a79e510a89f2d43ebdd6c8bf326d6d3b8af8d92c4cb12001172a7b5848c"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.3/xmux-v0.9.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "c1fb66811767f08655ecff01a56624e458e592ab509d43c9e7222e6096290b5d"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
