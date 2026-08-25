class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.6.3"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.3/xmux-v0.6.3-aarch64-apple-darwin.tar.gz"
    sha256 "1d12f06aa3f57ca98d911a069c969dc8dd4ab8b6494ac70815bd843be7785e4d"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.3/xmux-v0.6.3-x86_64-apple-darwin.tar.gz"
    sha256 "9c9d095ac60c7de186f50ee9eb4a7c02123f6d5beb655f51abbb0d80466ef411"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
