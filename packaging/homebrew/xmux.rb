class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.10.2"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.2/xmux-v0.10.2-aarch64-apple-darwin.tar.gz"
      sha256 "8b02b1fc89e73e8ee8ec3b063e5240e9d7cfef4ce695c0baedd5e9c96cb6cf34"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.2/xmux-v0.10.2-x86_64-apple-darwin.tar.gz"
      sha256 "643b00c566965411b1c551d96586d8ab261de9896761f5c3a0f098771ac572e2"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.2/xmux-v0.10.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "96748eb0a91d5d9b4371f468d42a3a39df613d194e3c90059c4368fd99bc06e4"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.2/xmux-v0.10.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "ae5a35364d91f38a66eda79868e2a7701bbb920ec24f948e4b40d872fb60016f"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
