class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.13.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.13.1/xmux-v0.13.1-aarch64-apple-darwin.tar.gz"
      sha256 "79f0969ef61414346957852d1dcdc26c41d5d5bfb7af2e47225bbc45b40a8004"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.13.1/xmux-v0.13.1-x86_64-apple-darwin.tar.gz"
      sha256 "b7b94dad2a327928dc964e472b59b33f9f1a97963b2a5ca95b2f7cfdbf126b8e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.13.1/xmux-v0.13.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "56414b9809cef1e90f2d62d0534c9e5bfdf4f3343173b83e3e5abe2f89a83e30"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.13.1/xmux-v0.13.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "ce916fb53fb4ca21bd5bf192ef578a72b475f287bd8ea2e7083f157940e0d302"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
