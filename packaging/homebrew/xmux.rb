class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.9"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.9/xmux-v0.9.9-aarch64-apple-darwin.tar.gz"
      sha256 "c850fc21f5d97d2b0592eef987b48993aaaad14a69c0a390bdceb3fb7ce37da4"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.9/xmux-v0.9.9-x86_64-apple-darwin.tar.gz"
      sha256 "1d8423259ae176306d698ab332de5cf6fe9a20f3cdbdce3786753bfd418d6975"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.9/xmux-v0.9.9-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a864150b08a853b0b39a38f2e086a2c380a44171dc1075f846a311db0af0745b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.9/xmux-v0.9.9-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "10776ca0f39881cdf37ddbd9159da31253e33beadc83a3449401ee65cacbae71"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
