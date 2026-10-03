class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.11.4"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.4/xmux-v0.11.4-aarch64-apple-darwin.tar.gz"
      sha256 "5a5d371fc2fccbfffdab9cb829ce6b58e6925fdf75e22a7df760eb42ab99e2da"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.4/xmux-v0.11.4-x86_64-apple-darwin.tar.gz"
      sha256 "9c254a4863a6bc6a8b272a9610d90444d3e17a4c0c7dc0520820a36d0accd86f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.4/xmux-v0.11.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c5b9c844293d00c965c6eb2b7d2ba2dc8cd58932999e11f6cac1a14b1397416c"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.4/xmux-v0.11.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "32a256258554807cc7e7fa9b9cab6e8851285e90fc66627c91b0aadeed0bac08"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
