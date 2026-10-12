class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.20.5"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.5/xmux-v0.20.5-aarch64-apple-darwin.tar.gz"
      sha256 "fa1afbff1d69ac6e98478ac8965b1a49663905e5eacf2b48af40489870cad465"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.5/xmux-v0.20.5-x86_64-apple-darwin.tar.gz"
      sha256 "25fd99c1b5d22290a11013d63336737fb129fb9137febecef26ee8e26de144a1"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.5/xmux-v0.20.5-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "705956452e5ed56fb1d8b4b23b4f2d1e80d810938f15fdc7dbc9e176a8ae9123"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.5/xmux-v0.20.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "47ab6d7568f0ecf6e8037bf9b224a5639e363796011e1b8d25ea913c56f87533"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
