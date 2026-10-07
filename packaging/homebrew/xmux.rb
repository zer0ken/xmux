class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.16.5"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.5/xmux-v0.16.5-aarch64-apple-darwin.tar.gz"
      sha256 "bdc4ccf585d0ea6b89bbc83819e771d9fafe33b2c4e07e98b459455d835571a2"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.5/xmux-v0.16.5-x86_64-apple-darwin.tar.gz"
      sha256 "5ffff6eb5b89b983552bd31daca8d5ac820397f404b366a3ffa60164342c2dc8"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.5/xmux-v0.16.5-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5b7244f09357841e830274adc6e96c25f3895cd341cc192468a290b21207a2ce"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.5/xmux-v0.16.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "77c9e2d24c9d10a14f5cf8195ccbd0e2d243b7bc04a403eac05434ad3f4ed057"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
