class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.8.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.8.0/xmux-v0.8.0-aarch64-apple-darwin.tar.gz"
      sha256 "c849c0addb1d5d3427c97d547a7a05f027238d2efab1d2296741f4fa04bf9fd0"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.8.0/xmux-v0.8.0-x86_64-apple-darwin.tar.gz"
      sha256 "cb6ec171784754da95a361734ca59ca8c6456573eee651657ca5f61d3d72de42"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.8.0/xmux-v0.8.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "cf101ae0f44a7a7be173077164bc3fe4d5676b3d54e71fe7d1988c5c63b753a1"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.8.0/xmux-v0.8.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "986f1a002597bbb67117781cb6db3f35121198871324afa76de21856a23d3b57"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
