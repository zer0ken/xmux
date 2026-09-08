class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.1/xmux-v0.9.1-aarch64-apple-darwin.tar.gz"
      sha256 "42deddf95d6e415315630f42657db4fc0fa456aaf891962fdabe213b06359567"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.1/xmux-v0.9.1-x86_64-apple-darwin.tar.gz"
      sha256 "5c339c528a1ad4f0cf3d05814d1afac5905ca12b3ef2298ffa30d03f47404068"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.1/xmux-v0.9.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "724a38a14cf2f815ae983b43bedaad7c5d8c3ab7ed18d6b9b37a6d370f681ffc"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.1/xmux-v0.9.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "ac741bd5022f9215bc719899e6033cf77cfc9044922e4e7d2cbd090e7697e0fb"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
