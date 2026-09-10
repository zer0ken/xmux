class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.8"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.8/xmux-v0.9.8-aarch64-apple-darwin.tar.gz"
      sha256 "a153481df43b4c17ebd860c6e4dda08267c9fc045b1478eadc0079a452347acb"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.8/xmux-v0.9.8-x86_64-apple-darwin.tar.gz"
      sha256 "834cf141765d507adbffa06cc5c02a9e1fcb181b3fe49a5efa33936b07f9aae0"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.8/xmux-v0.9.8-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a62fbc9a44e0a901c58ef1dc88e7d32d79b3e7460ec6dd4385fd7dccf666e7f2"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.8/xmux-v0.9.8-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5cde89c812733040ab1125a3000e7becfc4f7c8e1aea34b9ad8636fb312c6a12"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
