class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.0/xmux-v0.9.0-aarch64-apple-darwin.tar.gz"
      sha256 "95dca24b0be7d7c6ce9c952911ef19ccb708b2b6947bf7942411077d7eba27b8"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.0/xmux-v0.9.0-x86_64-apple-darwin.tar.gz"
      sha256 "972fa8f3bb8b3b213f6b11a62fff0e032f05425d06199f1662cac9fabf16be7e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.0/xmux-v0.9.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "49e084228822da0a0c5b398f690ba9d13bf0eaa4fa21bd17357e074782fcf822"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.0/xmux-v0.9.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "48ccb5445deff5b202b1f703f7859b7ac31726de3002e45f4605f1490551f006"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
