class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.14.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.14.0/xmux-v0.14.0-aarch64-apple-darwin.tar.gz"
      sha256 "67d32e47b93bbff45f7b47e0a633d5c8159f30c91c0518dd6863d263d5e20a4e"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.14.0/xmux-v0.14.0-x86_64-apple-darwin.tar.gz"
      sha256 "459308ca58b3b94c1355fe27dfb4faee90bd5bc7dd3f2efefebbc684122f5495"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.14.0/xmux-v0.14.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "1d0243ec8dae521399ae201bd50bd457cb25286db9d0fa44f4a0d18d7185b790"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.14.0/xmux-v0.14.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "648f38714db6b00f16867b06bce5b96e9e488ce4c0100e23abc13c6e54826124"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
