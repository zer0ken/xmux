class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.19.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.19.1/xmux-v0.19.1-aarch64-apple-darwin.tar.gz"
      sha256 "88ccc75638984fc7cadfdd375a55eeaa35d279513c1f73e0430b6f56b8e05a17"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.19.1/xmux-v0.19.1-x86_64-apple-darwin.tar.gz"
      sha256 "dccfa82eaf52bebbe38e69326d9032800b1104b68e8fe21bb6b13696d2fec79d"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.19.1/xmux-v0.19.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "150fab6a7d27d258def182eead95ced3006df03317f67a93a455eb1d7ec13a70"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.19.1/xmux-v0.19.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "11485106a474d5c14feb91ef9baa7014361738769e7d3f3a1f6f57f807e931c9"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
