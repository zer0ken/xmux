class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.4.0"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.4.0/xmux-v0.4.0-aarch64-apple-darwin.tar.gz"
    sha256 "REPLACE_WITH_RELEASE_CHECKSUM"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.4.0/xmux-v0.4.0-x86_64-apple-darwin.tar.gz"
    sha256 "REPLACE_WITH_RELEASE_CHECKSUM"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
