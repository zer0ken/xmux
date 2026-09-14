class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.10"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.10/xmux-v0.9.10-aarch64-apple-darwin.tar.gz"
      sha256 "a0f863c4989a234d8ccffb5c45a3eae0ee7c4a32857401dfa772262633ce8877"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.10/xmux-v0.9.10-x86_64-apple-darwin.tar.gz"
      sha256 "1383240feb895ccbfa14fe4de2598708d6d7ec5d33c7289c803bd52e6012b3fc"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.10/xmux-v0.9.10-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7bd9727a6640b518b5cb411cd36e97d176198c4a46481e48ae9d93219517699d"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.10/xmux-v0.9.10-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5dc147b530e8fd0216a9a12fc0677845e2e4ae6c09b41cb283eea208ac7f91c6"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
