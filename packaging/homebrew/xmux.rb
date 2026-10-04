class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.12.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.0/xmux-v0.12.0-aarch64-apple-darwin.tar.gz"
      sha256 "954f2ca0559add5fb0a5385eda4b2f254f771af82089cb5c2d84b8e50f6cd784"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.0/xmux-v0.12.0-x86_64-apple-darwin.tar.gz"
      sha256 "79b11c4e8eb43d5e13350e392429e0c9f84f1eaf8cc7285fbb2145e22ee549ed"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.0/xmux-v0.12.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "63699ab356d9d34e31fa61f64ac73f1d6c4cf19d5461dec4638612708a382322"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.0/xmux-v0.12.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "41a070450b5454140f57eb414d778a9c3cc1f998fcf639cfd7ff89adc208d630"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
