class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.16.3"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.3/xmux-v0.16.3-aarch64-apple-darwin.tar.gz"
      sha256 "5a1610f96660010a0e0a924de400c90178bd2d4ed34cc5deb5ca09ac35295818"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.3/xmux-v0.16.3-x86_64-apple-darwin.tar.gz"
      sha256 "c988fcb5479d149fdca829625da1653a1d69d8b04d79366f42c534b8d1bd6ee0"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.3/xmux-v0.16.3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "ee55825cd5b5b1350933a4fa334ac8687d4839642a71b58746ec2f387d2ded53"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.3/xmux-v0.16.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "73dc0a5cd1dbaf23f253705fe375f08d975c1cadc578bb1a76a86be72a6a818a"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
