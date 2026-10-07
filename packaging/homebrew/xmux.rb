class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.17.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.0/xmux-v0.17.0-aarch64-apple-darwin.tar.gz"
      sha256 "e099a61164c07039c2df1dd16aba1d983b967cc683e699258bdb3eaa81bd0737"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.0/xmux-v0.17.0-x86_64-apple-darwin.tar.gz"
      sha256 "47c1936edc74d28d09d34b3d1bb66be312e36effdabdbb1b4fe69d564158fa74"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.0/xmux-v0.17.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a31c51540bd8b95d87301791b4a06a7288c9c4bb418edcfdb0ece399233ccc1e"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.0/xmux-v0.17.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "45297d9f3ce7826430314ecc15e75d85f050835b3c85163611764132a823eaa9"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
