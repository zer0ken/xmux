class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.16.6"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.6/xmux-v0.16.6-aarch64-apple-darwin.tar.gz"
      sha256 "df96b723e872e1e2d6c1500faf1300e60d62019de6b19ad04efa114a98cd3e52"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.6/xmux-v0.16.6-x86_64-apple-darwin.tar.gz"
      sha256 "2137b2dacc459bc9db0ee4d56a2054befcf90b5d0fc12d5d6c5b79c426af8022"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.6/xmux-v0.16.6-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c98427db347047b1ce3936ef892fc7da38cc07d7dcbf0cb9596b82412d018c4a"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.6/xmux-v0.16.6-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "53aca9add9811465b72749ec4f446a3cf78feadb73abd5c3edd035a779e6fa68"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
