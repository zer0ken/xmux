class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.18.2"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.2/xmux-v0.18.2-aarch64-apple-darwin.tar.gz"
      sha256 "0e256d8ea1c16f7dfe9611706d34e4a0eb52b46f8f255c893df3466b3f5ed8d8"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.2/xmux-v0.18.2-x86_64-apple-darwin.tar.gz"
      sha256 "3f1c0407ff87c1ae37736a39183f28fdd45ada7093d0097ec76426cef704eb67"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.2/xmux-v0.18.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "dbdef57d722491c3eecd0f210defeadd85f079f26ddf6fffb3c43227dc162fa8"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.2/xmux-v0.18.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "29eb17858098eeb5955cb9874007d2ee098b803b2aa01ec70fab44f89da83dc7"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
