class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.20.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.0/xmux-v0.20.0-aarch64-apple-darwin.tar.gz"
      sha256 "363e4c71879b9b743144d5deb4787a6cccf4c05d398dbc6cb8d96b89bc65e1e9"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.0/xmux-v0.20.0-x86_64-apple-darwin.tar.gz"
      sha256 "bbc5438831dfd72ee5bc20db002663e41d9396fcf3ad0b4b3b1356348c98d792"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.0/xmux-v0.20.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "ca67bd5948baf6e73055046bd274ceafde6c6506919960629ed97e8155671612"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.0/xmux-v0.20.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "8daa15d00c1de9d337195a3411ede23f8a533912dbb3c151ba7278a959536a6e"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
