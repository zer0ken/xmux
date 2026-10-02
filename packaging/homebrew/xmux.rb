class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.10.5"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.5/xmux-v0.10.5-aarch64-apple-darwin.tar.gz"
      sha256 "15d9523b72a1f1c793bffa3267eba3c03a6ffec59b3054febed51b0ad680434e"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.5/xmux-v0.10.5-x86_64-apple-darwin.tar.gz"
      sha256 "fecf5015d23fa99622c3fdb98eae5c57917588c663947f226affc172a9074be4"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.5/xmux-v0.10.5-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "10eef501b1fd558cb51d2b25b3f679f2cd9f25082a5df890009918613dea1760"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.5/xmux-v0.10.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "83351ede29f2a1ebc562bbb2dc75491ea8297bc3429edac4f68723a14f0ce2aa"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
