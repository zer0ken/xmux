class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.18"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.18/xmux-v0.9.18-aarch64-apple-darwin.tar.gz"
      sha256 "aa8569a787394b0ca93701d11f856758626a05a8351aad8178f1f634658e7a80"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.18/xmux-v0.9.18-x86_64-apple-darwin.tar.gz"
      sha256 "f9ee785c6a898c000ce812226dacf19e21654e0cacebc9b2468ded89f9d1a61e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.18/xmux-v0.9.18-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "288ecba29327116943204a0082c1073ab85ecd62386c9e25d6852d5a4b39716c"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.18/xmux-v0.9.18-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "fbd18ab2e6c02eb7d33415d21f91e6c5242cec0e464cb7e3378971a84610f983"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
