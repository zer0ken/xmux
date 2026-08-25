class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.6.4"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.4/xmux-v0.6.4-aarch64-apple-darwin.tar.gz"
    sha256 "0c460915925fe4d914a811e900aa7b1d4df9288653e4d42003116bcb1ac16e08"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.4/xmux-v0.6.4-x86_64-apple-darwin.tar.gz"
    sha256 "fe094332c215a049b0361e32b199ba584012c4ad2e52ce8e5a3ec38629660ce3"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
