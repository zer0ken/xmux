class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.5"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.5/xmux-v0.9.5-aarch64-apple-darwin.tar.gz"
      sha256 "05123e061f425b96a14caaddafead1a1a15bf004ff8dc52fe3cdbe0a7e962d79"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.5/xmux-v0.9.5-x86_64-apple-darwin.tar.gz"
      sha256 "041fc7dc3765c4570ee3da555b9b93290dcefa7b95c89b5c09e9baaf5b61c272"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.5/xmux-v0.9.5-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "f1bb9e36e1c0e3601cb809668b4e84b1e88d9430cd1846d1798ed12762c5c3f4"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.5/xmux-v0.9.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "11d7bf52ee1ce4c339794c525ecf06f849742351fb735813d6ec2c8eaaa92606"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
