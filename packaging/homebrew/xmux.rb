class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.4"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.4/xmux-v0.9.4-aarch64-apple-darwin.tar.gz"
      sha256 "7e44968ee28764480c44d852475f0e1dfde30f9091a4c2710ed175bd599d9c71"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.4/xmux-v0.9.4-x86_64-apple-darwin.tar.gz"
      sha256 "6e7f62a7ecf14a3a4bfdce9570b6711939b238ce2513d9cf0854940f33c177f7"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.4/xmux-v0.9.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "dfaa2a3f6e792f3730d781285a31f5363a7cc8879d25b37a9e0595b178e75d23"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.4/xmux-v0.9.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "121eb7be2965e306b3b46f160217654ee25152c747aeadc1cd5f5904367c210c"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
