class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.16.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.0/xmux-v0.16.0-aarch64-apple-darwin.tar.gz"
      sha256 "588de42d710c1e9ab39a789775be8930daea3371266d434eddf3e62e3460f3e1"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.0/xmux-v0.16.0-x86_64-apple-darwin.tar.gz"
      sha256 "c456e710700e9cc6acbfcf1ea92f6352301d4ceb100a9bfdb857310cdda1b7c9"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.0/xmux-v0.16.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "fc4fb85313471dfeabad8a69bee4aded90d5e648ea333f10a7ae18ae2ef30130"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.0/xmux-v0.16.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e8ea1cd31a45e7af3c25330e494e68b929c29935b5fb53e83a32032dec52e039"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
