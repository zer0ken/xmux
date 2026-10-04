class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.12.4"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.4/xmux-v0.12.4-aarch64-apple-darwin.tar.gz"
      sha256 "e6fd458718d4a218e416ec38ca3a265ae6a8e6dc277ab0027f9e155bcc2521f4"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.4/xmux-v0.12.4-x86_64-apple-darwin.tar.gz"
      sha256 "80ef252bfdbebfdfa9a9103bd701f6eb8b8648c799438a13b768ee7dfadce14b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.4/xmux-v0.12.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "80370f051f8a8dfe17a89cea991fa751ba531edaf1858c7467c45a3dd91b9121"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.4/xmux-v0.12.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "50c2cfe09a05b533dce58de6dc06df2edfcc714f5e3402fec4c71d2be76ac639"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
