class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.6.6"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.6/xmux-v0.6.6-aarch64-apple-darwin.tar.gz"
    sha256 "4fe579ffc356f1ae47eace9db422d4514a850ddc79cc4e0cd4a17ff65c4349e3"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.6/xmux-v0.6.6-x86_64-apple-darwin.tar.gz"
    sha256 "a8777bd2aa4fd58380cfecca6db3e98d84a27c9d7790513e998508d30cb42a97"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
