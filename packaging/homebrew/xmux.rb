class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.16.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.1/xmux-v0.16.1-aarch64-apple-darwin.tar.gz"
      sha256 "574d402e3a8ca534e72fbb0e0c591c7514c7bfd496a48e03040757c14f5753b9"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.1/xmux-v0.16.1-x86_64-apple-darwin.tar.gz"
      sha256 "f51078f5438496294e0d461938e7e3c5aed863fb71951062ca18c722c6882c61"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.1/xmux-v0.16.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0b7314e688d272430f98e25790604c4dde1a8511575a8c5b83f2e8a2c0abb72b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.1/xmux-v0.16.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a1a98441aa456b93197ef5e0702828f4f7edce548f16f2957aa2004409a43277"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
