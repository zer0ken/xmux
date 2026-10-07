class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.17.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.1/xmux-v0.17.1-aarch64-apple-darwin.tar.gz"
      sha256 "e9e40da77477b7f7a1021cdc538951b66aacd11adcf2327f80fe3edce700238c"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.1/xmux-v0.17.1-x86_64-apple-darwin.tar.gz"
      sha256 "d4b9f655feced2657b92ffe3aaadd549e02466c06575d0402ecf4e7196d54a1e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.1/xmux-v0.17.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "4565781922163406e3bf283f9e899787d0c79f65d8d63da5b0a144171035b4f5"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.1/xmux-v0.17.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "574590d0d45eef50942e5716eccd2bc31254b321f76d0cd1127108d1805cd814"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
