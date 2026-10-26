class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.22.4"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.4/xmux-v0.22.4-aarch64-apple-darwin.tar.gz"
      sha256 "032f6186a9fb10969d129e148840397452c2c183e37438ea6f9f7f72af590506"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.4/xmux-v0.22.4-x86_64-apple-darwin.tar.gz"
      sha256 "65b32cf030c3ff1335e4a7253c1eb56e80e4a35d6f4984ced6340d42cdc07dcf"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.4/xmux-v0.22.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "861fb7533c63cf89d3b401a8f4911d4a462284f75661d673b2b043c12c4b3f69"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.4/xmux-v0.22.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "93d41b1207d95b4dd3ce3c629374c37183fe72d993a1fa2c1ea4a41177051788"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
