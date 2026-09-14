class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.11"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.11/xmux-v0.9.11-aarch64-apple-darwin.tar.gz"
      sha256 "b915c2a38f26ef4305029eb862cc501678dbd9fc40e79c9de4967f727d6a2555"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.11/xmux-v0.9.11-x86_64-apple-darwin.tar.gz"
      sha256 "fe075c07d49cb142df5a7e2feda8494f1a16fe74ba2d8c85f4c60e95a528aea7"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.11/xmux-v0.9.11-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "df3d2488a5e5ff52989a3101082cdc85900f999b12242969a848e74793e19935"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.11/xmux-v0.9.11-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "cbc789faeedd34adf865a877c7982368ad09ac4f45376fce4cc7f5736914bf29"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
