class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.21.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.1/xmux-v0.21.1-aarch64-apple-darwin.tar.gz"
      sha256 "d7ace2c0047ec199f8f13762c8cc441fe911d8b5715c6e9a40c9c9cfb2132a6a"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.1/xmux-v0.21.1-x86_64-apple-darwin.tar.gz"
      sha256 "f7e0e8cedba1ffe9114f296e7d6fa30c844b7ae0a1257dbfd9481a24f0284141"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.1/xmux-v0.21.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0dab6587fa0de1f13946709d125763f9bbe097b17eed763fe3e909add33acc62"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.1/xmux-v0.21.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "3abce002cc8f84efe9174c9d68177d18a8440ace7cb4b44de11e17c1f5ab079d"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
