class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.22.3"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.3/xmux-v0.22.3-aarch64-apple-darwin.tar.gz"
      sha256 "fb6319087ad05b70944149f855e1a0ed7f8c7409650a97415708ce3ddeb8927e"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.3/xmux-v0.22.3-x86_64-apple-darwin.tar.gz"
      sha256 "1387eed63af8f925b8104a0c9cabe20ed0c6b3b9c4fb81e6b41175aab524c25d"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.3/xmux-v0.22.3-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a3a0cca52ca633105a58066ac628c0cfc4fd70ff1fffde9700a55860bbc03536"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.22.3/xmux-v0.22.3-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "90129913a0d528074d0db089c4a250d1b9d9185aae5d46013a779c64fb0f89b5"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
