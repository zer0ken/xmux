class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.2"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.2/xmux-v0.9.2-aarch64-apple-darwin.tar.gz"
      sha256 "05a239ea8c026b6b4148df5a6a4da470d3dbf1d1a82341dfff78769659e4ade0"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.2/xmux-v0.9.2-x86_64-apple-darwin.tar.gz"
      sha256 "d8389467e35e57bf5a0630b071008790e01f5987687f07367407e4e998866c9f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.2/xmux-v0.9.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "1d5c96d1e044897be45ceeb50cb3b8554117903a0a4a56631c03a968fabae0b1"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.2/xmux-v0.9.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "1881ae98e25bf7a0a99d38750643b51cd26600dc85ae0f4e00eff17b03123070"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
