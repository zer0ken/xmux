class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.21.2"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.2/xmux-v0.21.2-aarch64-apple-darwin.tar.gz"
      sha256 "70096b52c31b8c68f32cc7557421b60f30822f69fdb038c5e3b133b87cccae5d"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.2/xmux-v0.21.2-x86_64-apple-darwin.tar.gz"
      sha256 "7fd9821587a52c9ead713e3d6588eb1d16b70db99bb2d11d6e7658a5be82a974"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.2/xmux-v0.21.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "f9a7fb5d252d7b269efd8e41eaef4535fb3c8041997ebe566353394bc267ab61"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.2/xmux-v0.21.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "96daa445a8466cd0ab432287e51c38eabe624a6f2aed55cd911540b52a711d65"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
