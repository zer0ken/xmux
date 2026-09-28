class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.19"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.19/xmux-v0.9.19-aarch64-apple-darwin.tar.gz"
      sha256 "ceb56fb45e64ef50ed6d39d93b2057c19b2102218023735dd1e0156b5773033a"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.19/xmux-v0.9.19-x86_64-apple-darwin.tar.gz"
      sha256 "34560c33ee069316e785af0ac7c877f524424aedc328d71748bf565cbb12b3a0"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.19/xmux-v0.9.19-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "403b4250c1e6ec8cf2c163a6e53605dc314d10277e9efa7df6c889fb33053c1b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.19/xmux-v0.9.19-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "84dcbe23865116725c73a3512944e07c8d062728d997499a538cf842e6e910f5"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
