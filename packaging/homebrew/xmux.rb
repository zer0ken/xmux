class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.7.1"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.7.1/xmux-v0.7.1-aarch64-apple-darwin.tar.gz"
    sha256 "a171b077089545afbcf1a367525306d805493b46f49568fc1872b18d0a6392c3"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.7.1/xmux-v0.7.1-x86_64-apple-darwin.tar.gz"
    sha256 "050e256c61a9eb4069b00cda21d318e54c8388237da680297015645c8f07d154"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
