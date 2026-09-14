class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.13"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.13/xmux-v0.9.13-aarch64-apple-darwin.tar.gz"
      sha256 "a77a46ab17b9e778244c606d1ad66feb2d385de08bb93d80ed7a895fbc8451b0"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.13/xmux-v0.9.13-x86_64-apple-darwin.tar.gz"
      sha256 "6d20fb01e1cb7a34566f88dcdc73da95948eb7892343c94355dc0f403339a951"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.13/xmux-v0.9.13-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "2594e898467c13cf847a25e940f9e2c184b301e1d396e2fa446f6bdfbc12bec9"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.13/xmux-v0.9.13-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "97933ac6607d096b941782af986cfa9dd5aa8fb635137c8a6ebed37d1937e827"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
