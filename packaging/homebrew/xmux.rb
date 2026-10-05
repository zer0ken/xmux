class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.13.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.13.0/xmux-v0.13.0-aarch64-apple-darwin.tar.gz"
      sha256 "e51c5bfa91d73ddd018aa8f1c717c025893014e25d25a3cb90c49029bfcf31a8"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.13.0/xmux-v0.13.0-x86_64-apple-darwin.tar.gz"
      sha256 "2a8d66575d106029db285f0934fe66070d4b629c86f40098c7c9a297e8a15259"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.13.0/xmux-v0.13.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "ad42fe3819d38cc4a5d5ce6f62a2a175b276ef06c756d9592fb75e85069c3c76"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.13.0/xmux-v0.13.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b87556ac871270cb8af15bc0cf23abb2a20c64672a5a12f484bd902d7eff179b"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
