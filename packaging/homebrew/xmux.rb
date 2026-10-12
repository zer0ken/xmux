class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.20.4"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.4/xmux-v0.20.4-aarch64-apple-darwin.tar.gz"
      sha256 "0664c85f999ad70a97a543af1bdc50bbbdce58944bba66b7683158fde813b45e"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.4/xmux-v0.20.4-x86_64-apple-darwin.tar.gz"
      sha256 "67e4648b0bafcc9e261ab1849d8baade1a30baaa797244ce2b30980528aa01ba"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.4/xmux-v0.20.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "4b444b180944710c17c34361d898cc0e84e8acf5684c44468487aa5b6385ca9b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.4/xmux-v0.20.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "a762b1e97ea5972dad1ba510b1f50e95d7bc0b1e4dedc8e4966271abce2bdafe"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
