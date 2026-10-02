class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.20"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.20/xmux-v0.9.20-aarch64-apple-darwin.tar.gz"
      sha256 "db2c1e759af7dff1dc25b2a044a08fe9ed9af8d1017109aaf46301f454b46058"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.20/xmux-v0.9.20-x86_64-apple-darwin.tar.gz"
      sha256 "1386be6aa3ca99d4de9de9d1fc84e6ba3d4cb3c16a7ab89f8c90ffcf9b22ec0d"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.20/xmux-v0.9.20-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "844fb928e58aca32fbad48437ef7b322d8564d0296354348ed08af037408b0f7"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.20/xmux-v0.9.20-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "cacf457fef58b01d8b603a2df78f01413bdaede65da4ca3a34284cd2dadac69e"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
