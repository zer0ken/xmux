class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.20.3"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.3/xmux-v0.20.3-aarch64-apple-darwin.tar.gz"
      sha256 "811cce94bfd8c9171d9cdbc05716d8f04d27cac7d2bd1f0935d9a0ba44f0e759"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.3/xmux-v0.20.3-x86_64-apple-darwin.tar.gz"
      sha256 "d367b19664c3ed61264b0cc41eead26e9553c8baff703cbda2fb62948fdd4c8d"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.3/xmux-v0.20.3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "91af70cdc839ac4477d37e2845445adcea52dad42dc629a3dcdc27378cb3c8c0"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.3/xmux-v0.20.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "887bef6494f006e38f3de8e7b15602e97dda3aa9e7f67fb4c090f43b4fce49e2"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
