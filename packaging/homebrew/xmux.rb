class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.11.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.0/xmux-v0.11.0-aarch64-apple-darwin.tar.gz"
      sha256 "44f25d6443a749277834a0fdfc8e5195acbf5bde8ecae517e8b5cb1c0daf2e83"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.0/xmux-v0.11.0-x86_64-apple-darwin.tar.gz"
      sha256 "9752101cc213a57b3da058dea1ead1baa8d5cb1f807ef9b13a563df890bc0375"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.0/xmux-v0.11.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "1def3b87f9b37ad5d5fe0388ffe2f4d4c3c8b48d51a4ae2b343d1f657e87c70a"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.0/xmux-v0.11.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "62c331bb7fdf2982bd2d871081019482f01fcd918193d492cfd5b12cf7e8f1ae"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
