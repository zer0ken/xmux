class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.16"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.16/xmux-v0.9.16-aarch64-apple-darwin.tar.gz"
      sha256 "d38e0d468a5f28033e9da05dcf5e59298b485b78fe6f981c9b1404d7bcd41ed9"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.16/xmux-v0.9.16-x86_64-apple-darwin.tar.gz"
      sha256 "b8fef3382cf55061f15b799e064eab14d5c35b1d4f22b28855d31114ebccd246"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.16/xmux-v0.9.16-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "b4b854fd264173c2f4e875f920e3e703aaa7f410d9d4fbea961509b5abad6391"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.16/xmux-v0.9.16-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "15e4fe9452cfc33c73ad55fce383b8c09a7886c2ef8b9e387a50b803f15c95d9"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
