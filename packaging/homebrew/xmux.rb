class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.14"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.14/xmux-v0.9.14-aarch64-apple-darwin.tar.gz"
      sha256 "3225430dc6fd49783ca598592b087b9c3ccaf0ca15ebd3c48e6874798df4326e"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.14/xmux-v0.9.14-x86_64-apple-darwin.tar.gz"
      sha256 "eb8b806035b8ac48c4354f35fb4be934e0e6471387d0f5088a9e2187eee0e22d"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.14/xmux-v0.9.14-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "8799cca01d85987f76b16702d47243b6c660212dcee3faf80052d7290cff3efc"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.14/xmux-v0.9.14-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "8b1279f408e9af615b3d52cbe872ed93cbc2084dec566f190edeefa72e841ca5"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
