class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.22.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.0/xmux-v0.22.0-aarch64-apple-darwin.tar.gz"
      sha256 "50c7b146bcd34048a3eb5e36fc00c47de4a9bf3b079a8a632d1da379b64a49fe"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.0/xmux-v0.22.0-x86_64-apple-darwin.tar.gz"
      sha256 "77aba38c453b2c44fa101c152e429d704ad05665bba63014290e5f9285cbe969"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.0/xmux-v0.22.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "21334b0dd4462a1a92527c88542a1e503309cae108dc72b048f54969e436e0fb"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.0/xmux-v0.22.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e4624608e5f207a6c0e4a9334ed2f50b5062255ed0102d93448bff454bbedccd"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
