class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.12"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.12/xmux-v0.9.12-aarch64-apple-darwin.tar.gz"
      sha256 "c6ba3aaff0959d515655f8d9750e575301d4185f97da3cd75cf70bb723de9ec2"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.12/xmux-v0.9.12-x86_64-apple-darwin.tar.gz"
      sha256 "aa12dcb453d3023fc8679235c10ca93874200bde0e6465bb9de8db970917c3c7"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.12/xmux-v0.9.12-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a4c47619a0dff534aa2ef9a5edafd5cccdb458e6c731f8f90a8ae363a5cd8d7f"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.12/xmux-v0.9.12-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "dd2edf65e0ac074229320c34b66f5bd9674a36a8dc9d361e615879c4139fdc03"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
