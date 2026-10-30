class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.22.6"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.6/xmux-v0.22.6-aarch64-apple-darwin.tar.gz"
      sha256 "863b6e25274a785d2c2e30c4d846f59d26390873890ee86d2dc6dfb9fa04885c"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.6/xmux-v0.22.6-x86_64-apple-darwin.tar.gz"
      sha256 "9054d22f885f2335b34cefc9683baba28812bcae1be970798743ac52a38e7c4e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.6/xmux-v0.22.6-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7411788f720db378f5be28a4248d94168b5ee8b00788ba4ad382010be8b17680"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.6/xmux-v0.22.6-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a10a74bf8f5e2cd272f306e7193ece0092b3bc38a246c614ab028fceac1f3f1e"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
