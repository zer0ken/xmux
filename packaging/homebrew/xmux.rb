class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.10.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.0/xmux-v0.10.0-aarch64-apple-darwin.tar.gz"
      sha256 "c281f7d361f4e7313f3ea4cf93cac48d5b9d75d5fcd45aef48fd2d8a2f4201e0"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.0/xmux-v0.10.0-x86_64-apple-darwin.tar.gz"
      sha256 "a3a2237fa41fe15ff34ff9c171bc599a7b5283154b8314d33edd13a9cfb10a51"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.0/xmux-v0.10.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "69b7e62ceb9bf5ca7ff207b958a65b6ce2983dfb075b090ba66dc07dfb495663"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.10.0/xmux-v0.10.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "80a4ae1ea5bad85de193184e68321d99e8dd92f5433f4bdd45eac986631084e1"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
