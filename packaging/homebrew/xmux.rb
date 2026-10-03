class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.11.3"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.3/xmux-v0.11.3-aarch64-apple-darwin.tar.gz"
      sha256 "85b6b367fe3bbadcb2db25070d5db8a6e21f0af31a59cb3b448c24c61086f88d"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.3/xmux-v0.11.3-x86_64-apple-darwin.tar.gz"
      sha256 "b66859a1e56d5b14fa1104a997e1692f36f6300bd09351f580f2f54c17513004"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.3/xmux-v0.11.3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "40b7a6eefdcefe2cec2120486ffe5131c047157b7901254fbbc1ea4f9321f0bf"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.3/xmux-v0.11.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "c4224a75270fcc9131f97fc8a6ae7e0f0551a64a972295ac3d6c2da1634bb1a4"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
