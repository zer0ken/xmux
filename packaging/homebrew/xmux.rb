class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.22.2"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.2/xmux-v0.22.2-aarch64-apple-darwin.tar.gz"
      sha256 "144fa105dca6e41255d6c5f0e7a39e039d9dfb658156d820c2bfd8d1fbcb9e7b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.2/xmux-v0.22.2-x86_64-apple-darwin.tar.gz"
      sha256 "c189ffbf7627bd6137ce1ba8c6fa941252098fe7eb4c22242d0fc8b8573f1870"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.2/xmux-v0.22.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "1927ded10858bbfd0978b9de7d484f022bc452a9ed27659725b947618cfce47b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.2/xmux-v0.22.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d23c82074a4ea61567c2028ed842ca8be2d2370577dc3b02e4063f8f56239a95"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
