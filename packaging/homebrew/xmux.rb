class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.10.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.1/xmux-v0.10.1-aarch64-apple-darwin.tar.gz"
      sha256 "b4992c87156197404bb2fa190cd0cd9b8b635dd27796b0896deeb93fac574903"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.1/xmux-v0.10.1-x86_64-apple-darwin.tar.gz"
      sha256 "a7ff7c4dd9b0cb86122b112b58cff90d8a49f69f0772a493707a4fc5ea378eca"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.1/xmux-v0.10.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "6e5963f94d95fc0f1e2721a4ba875312753c02a6f4bee112f7c0a57ef21a7307"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.1/xmux-v0.10.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5d757bbff0c4404411fb54044edbf224a27593e8e0b5c5afbaadabc319f60053"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
