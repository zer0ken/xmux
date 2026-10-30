class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.22.5"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.5/xmux-v0.22.5-aarch64-apple-darwin.tar.gz"
      sha256 "c66e2885edb1032782bcf7b0e32c0197e245a72d4618a10f904a12b505a5838d"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.5/xmux-v0.22.5-x86_64-apple-darwin.tar.gz"
      sha256 "a26fb24e9b1d017d2fa836fce735f92ca2b53733793b2330dc4334fa43e9ea4b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.5/xmux-v0.22.5-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "44b1d942d55e34eca16b823965dfe82c7b7f315c6e6b285cfba171875a17aab3"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.5/xmux-v0.22.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b687f201572ccff89b178a06abc390f4ae6097c98d8393ad78b540071884777e"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
