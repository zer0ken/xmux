class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.6.1"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.1/xmux-v0.6.1-aarch64-apple-darwin.tar.gz"
    sha256 "7d9dd85b38b15a8c3cc631a978c0c037a95d1c86d3fe97b19a5b3ac915415f3a"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.1/xmux-v0.6.1-x86_64-apple-darwin.tar.gz"
    sha256 "623c406ee7a2327a292799869be682c37c08d0ed806cb13a64e4006f11c0776e"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
