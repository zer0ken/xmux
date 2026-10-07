class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.17.3"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.3/xmux-v0.17.3-aarch64-apple-darwin.tar.gz"
      sha256 "e65ae8e8128b0c1495c3f1fbfe21448675c3a36e368c68a12942801b3056647e"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.3/xmux-v0.17.3-x86_64-apple-darwin.tar.gz"
      sha256 "7b1ecefdf44edbc63b48fe124367e8153b9cfd55f84ecccebf329c1bbb1f9f90"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.3/xmux-v0.17.3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "8395253e92e6d617886a9027eddc99d88f95b3805214729ee23e7ca7823ddf48"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.3/xmux-v0.17.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0e21ae148de398e604116ab9fec7ad0a293ca746f035051c0eb3d62653d36828"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
