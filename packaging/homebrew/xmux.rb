class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.18.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.0/xmux-v0.18.0-aarch64-apple-darwin.tar.gz"
      sha256 "833406ef12b81519db45d80a11fc132e2e84920b392f377a88796adc32735cd3"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.0/xmux-v0.18.0-x86_64-apple-darwin.tar.gz"
      sha256 "f339173cefcf175a48df04bb1d89295c59466446d29a6d625f2eccd909572ebf"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.0/xmux-v0.18.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "1e89f35cac4a5c96cc5ae9684b50d94cff68279794a7464a5226e2d4c8c1057e"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.0/xmux-v0.18.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "9ff8516f28f389ca9a8bc67321b3f72e32b65f21d51f71d2d6a6ffa00c10869b"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
