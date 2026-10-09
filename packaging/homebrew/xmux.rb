class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.18.3"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.3/xmux-v0.18.3-aarch64-apple-darwin.tar.gz"
      sha256 "59b1fca31915600b9b0add562ff83d35762821fbc8825f16f1c202b431bd035a"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.3/xmux-v0.18.3-x86_64-apple-darwin.tar.gz"
      sha256 "98d786f45f3b5f69df3eb156df92becba25b9e2a386615dda930fe05a5c899b9"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.3/xmux-v0.18.3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "acb2e89a0482e515bcd4a6f64a4f0e43982ad010512c79252fdfe954e93e6f3b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.3/xmux-v0.18.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5782c58b4acaad10b7dd925daba870a23e10961d52f7540ebd20d2f3f96f4a90"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
