class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.22.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.1/xmux-v0.22.1-aarch64-apple-darwin.tar.gz"
      sha256 "408b76737b09484d448288a67d21402d91330afbc81219ea33958e86305396af"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.1/xmux-v0.22.1-x86_64-apple-darwin.tar.gz"
      sha256 "a0b600e6503c76b0a4c1500855124130b9ae7f8ce21ca03848db34f9201892d6"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.1/xmux-v0.22.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a2db169e61a38fa0200033e834f265e89149c9b0a20aa0f104ee392fb83fef0b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.1/xmux-v0.22.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "30ae06df14f39e1b96af7bd9ea4378270c30f69b618cdfb15e98395e10350331"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
