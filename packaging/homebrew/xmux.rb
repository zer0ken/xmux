class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.7.2"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.7.2/xmux-v0.7.2-aarch64-apple-darwin.tar.gz"
    sha256 "a5d6ecc3668920149d64999bbeecea2c2bacd13b6d133393acdecf47a2569263"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.7.2/xmux-v0.7.2-x86_64-apple-darwin.tar.gz"
    sha256 "fc0b6d1638059a4822a4416b53a2cf1845e69deb8d441253646c9c66dd236efd"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
