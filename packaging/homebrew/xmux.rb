class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.12.3"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.3/xmux-v0.12.3-aarch64-apple-darwin.tar.gz"
      sha256 "1ea9f3f1d7ecc70e86c905ca6e8cf44973bd4f9837828894f550d93a766fbf8d"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.3/xmux-v0.12.3-x86_64-apple-darwin.tar.gz"
      sha256 "b88bbb1073aeaa2f3fc3bdb972389fc3adcf074e9d7514bc53abc6780d51ee5f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.3/xmux-v0.12.3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "aeea679b8e03c9e63cb19c0204e28e666703ea317b68f7ffaef4a4fd5c9c7345"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.3/xmux-v0.12.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "666d61ca4d7f059213c81792dcbb0e76033135c9844d6e6b9382bc8e8bcb3e7c"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
