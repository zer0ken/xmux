class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.18.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.1/xmux-v0.18.1-aarch64-apple-darwin.tar.gz"
      sha256 "c74b759a3f02d60939ea72f8d8eb6798033d2b6a9bf2c153cdfa69024c41f676"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.1/xmux-v0.18.1-x86_64-apple-darwin.tar.gz"
      sha256 "fe2486b50a1e733949a9342779d3aa6327c7bb7eb90c724ca1652ffea752af07"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.1/xmux-v0.18.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "2e491a66b8c107e3f9cf294d8b1d8049681d8d89d346da86aebcdbda65892ddb"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.1/xmux-v0.18.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "f4b77c36cdb2cd06385f7a5c0123ac2111b1823a334fefcef67bda34475da92f"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
