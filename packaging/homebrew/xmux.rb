class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.6"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.6/xmux-v0.9.6-aarch64-apple-darwin.tar.gz"
      sha256 "ff18c3ad0d67c7320d459c355739a3c2b762e9ffe57c2ab604fd8bdef12dd52f"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.6/xmux-v0.9.6-x86_64-apple-darwin.tar.gz"
      sha256 "26deb679bfa5ebc89d2a2dd13f231f0c56aa34db3f6f62385e85bd8a2b9217b8"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.6/xmux-v0.9.6-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "7c4864052ee5e5969186ba4fcba0da648f3fdc994c659792ff7d470c4b0b1e15"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.6/xmux-v0.9.6-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a09252596f074547d5ffca229be0f91b159155c8d741cdd87dcaa4e251f6b5fe"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
