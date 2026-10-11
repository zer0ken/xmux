class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.20.2"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.2/xmux-v0.20.2-aarch64-apple-darwin.tar.gz"
      sha256 "2a0a8afc9495189323416af84c1ce80c27b5b08e98cbaec8ff71c5cba54178cf"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.2/xmux-v0.20.2-x86_64-apple-darwin.tar.gz"
      sha256 "5a8b1660170638e69209118fae6d7c6b8c5f4d4f94b26ececcfd94c8c7e5ed1a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.2/xmux-v0.20.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7593e45bcb9ac6507c9ab578bfcd95c5e0acb959fc74e5b5543b09b8e4e1d50f"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.2/xmux-v0.20.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "2c9d8c4c1ba9463804de47ec9fa96f1b8bf5c5a32a1fed77cf7d0f5d91b6e4fb"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
