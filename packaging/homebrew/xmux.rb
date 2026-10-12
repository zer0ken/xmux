class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.21.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.0/xmux-v0.21.0-aarch64-apple-darwin.tar.gz"
      sha256 "90dec838b8a622a16bcbdae7357abc5ea4e0bb3a071033877394eee140739c15"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.0/xmux-v0.21.0-x86_64-apple-darwin.tar.gz"
      sha256 "aa4e2e8cf07d57c1b2efb46491d7bf6d5cda2981908244346cf5dfd86216c591"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.0/xmux-v0.21.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "81290d77ba6fa310069e75da1811ecd3f1272d0a3be4dc762b0f00a0682c49bc"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.21.0/xmux-v0.21.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "85eb9a496d6ab6cb8349e6537b08c4be17f7794d179442d1fcf24c0767778524"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
