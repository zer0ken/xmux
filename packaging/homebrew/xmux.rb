class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.6.2"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.2/xmux-v0.6.2-aarch64-apple-darwin.tar.gz"
    sha256 "15fff2607e66932adedd3af954aec0bb36792390c53e3e7bce86bfa76936d4dc"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.2/xmux-v0.6.2-x86_64-apple-darwin.tar.gz"
    sha256 "3f384ad01228e9c15a11c830abc50711ef26921f9b855c1f27bed39203449964"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
