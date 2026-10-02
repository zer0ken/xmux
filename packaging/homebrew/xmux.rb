class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.10.4"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.4/xmux-v0.10.4-aarch64-apple-darwin.tar.gz"
      sha256 "37f75c8a719eb7d6d3b162cf628a2225f47096bea726700783ea2bab0864b09d"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.4/xmux-v0.10.4-x86_64-apple-darwin.tar.gz"
      sha256 "03fc6fc6ae6fc3e02d5b97b5e658fc10556ed31d1319cf7ba2b52d1519cddc80"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.4/xmux-v0.10.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "3888b4c6e5cd5fed51548ac09785709ad24ee043426b5c47cc2e36c57b164143"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.4/xmux-v0.10.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "88f090e9cae8538fc15d716e6a9cc2c3892e839a76d9831998945e0a07d39d5b"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
