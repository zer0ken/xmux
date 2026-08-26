class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.6.5"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.5/xmux-v0.6.5-aarch64-apple-darwin.tar.gz"
    sha256 "29999295e3776629c3a62d1451412b98d01bf3c0a8c547bd2dcc338b6b84ec8c"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.5/xmux-v0.6.5-x86_64-apple-darwin.tar.gz"
    sha256 "3c8fd4a139052b1e388c3e37e4731c4531e1c585ed60918827b7d9a1c54766ec"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
