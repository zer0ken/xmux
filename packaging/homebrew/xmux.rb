class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.12.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.1/xmux-v0.12.1-aarch64-apple-darwin.tar.gz"
      sha256 "5c4eb2a4585f1e742990e3778579c697f011b85c7d7716a6500305aa2ff1f39b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.1/xmux-v0.12.1-x86_64-apple-darwin.tar.gz"
      sha256 "139679fae9c7bc87ecbdf311c3b595b66c65be7e5f106176dcee283b9f638f39"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.1/xmux-v0.12.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "be9a79be74a2dda48367172d28a7497af8e4cc46c151ca7992ef501a53029830"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.1/xmux-v0.12.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "3b31c63278a29309e095b4a9e4e9c6f20f63ffbc2b3b235a9e06229017fe8d01"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
