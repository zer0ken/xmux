class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.7.0"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.7.0/xmux-v0.7.0-aarch64-apple-darwin.tar.gz"
    sha256 "5982a2662be634d01251e27295b1da4585c3b09a5d4cbdfabd76ff917c0bc879"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.7.0/xmux-v0.7.0-x86_64-apple-darwin.tar.gz"
    sha256 "a6493534f53afe9201df9af68511222788c667daeacc06a657a99c73ddfa8c8a"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
