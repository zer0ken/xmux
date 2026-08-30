class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.7.3"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.7.3/xmux-v0.7.3-aarch64-apple-darwin.tar.gz"
    sha256 "945c4063d1cf9eeb4ab4c2cd4899b3ac7ca35cd6c79436e8887e5f1a4b48f03e"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.7.3/xmux-v0.7.3-x86_64-apple-darwin.tar.gz"
    sha256 "abb75121d8009deca7665fc81c3c45d57a44027d263be349186dc32954354b76"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
