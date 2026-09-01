class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.7.4"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.7.4/xmux-v0.7.4-aarch64-apple-darwin.tar.gz"
      sha256 "ed25d7fb04029b3cd3333e257b72f1c41e0528fe30da6d62547a8ed606d02ec4"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.7.4/xmux-v0.7.4-x86_64-apple-darwin.tar.gz"
      sha256 "9e743e95cf42ac7d7e9347d0a99e934300191eae09d4b2ccf6887d7b2c629cfa"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.7.4/xmux-v0.7.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "448fa9d65a8484576f04ffd55cef5d41da95a08679d27104d31816dcd7fc574b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.7.4/xmux-v0.7.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "30433ab6a8bd24564ae7d07f8bde2dc3a950ffa1bfbf4d6db34886140ab64854"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
