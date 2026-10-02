class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.10.3"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.3/xmux-v0.10.3-aarch64-apple-darwin.tar.gz"
      sha256 "e3a3c9768355e706c706172affb046ec64c9cd6432fd4f18bcc4c2414847a9b2"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.3/xmux-v0.10.3-x86_64-apple-darwin.tar.gz"
      sha256 "c62d6f0185f847ac27a39145c8dcd1db7bfcce6b9696f8939c58d9ecc2c50a66"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.3/xmux-v0.10.3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "544b3eef7ca7b2a8613195a9bd2c8a162d7d9a409ac620c8a78a7278a853d0c5"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.3/xmux-v0.10.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d9a952bab888f98ef8da505e5069127940fe3a7a34a80fb9da5cbba12331ff6c"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
