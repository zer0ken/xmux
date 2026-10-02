class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.11.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.1/xmux-v0.11.1-aarch64-apple-darwin.tar.gz"
      sha256 "11cc597687ca31b429497bb90df70a84e7dd28e24f22c248e17d2a16f33d73ac"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.1/xmux-v0.11.1-x86_64-apple-darwin.tar.gz"
      sha256 "d6e82ffac8a82bdb9effbb8f022e602250be4510500128518690c93c1a51f109"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.1/xmux-v0.11.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "3ddd9c9aeaa2cba1772cb2b1fd14b3184542b95b40874b59bb2858a1b857520a"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.1/xmux-v0.11.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "aa044bd823dac65c7628fbba63d78103177cfe75be94b9d0612d62769599d4ac"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
