class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.15"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.15/xmux-v0.9.15-aarch64-apple-darwin.tar.gz"
      sha256 "c39f69fff93c6a78df1d24d9ebf81841d2257cc7242c623434cd4fd7268a180f"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.15/xmux-v0.9.15-x86_64-apple-darwin.tar.gz"
      sha256 "ddab6875f47d7b6c2186715963bdc479a86aaea3b8abf3a18c26cccb31512fac"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.15/xmux-v0.9.15-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "02e7ab5ce5d1f0e98dd800854157a142a20e6c3309fcf420c6f342382173b6ad"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.15/xmux-v0.9.15-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "15042fbbd006ec5acbc133e99a4acfff93f55300daa2211e6f45f423e8f74917"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
