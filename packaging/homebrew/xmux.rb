class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.17.4"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.4/xmux-v0.17.4-aarch64-apple-darwin.tar.gz"
      sha256 "fa7e0bad9555fdf6660ff5225e935a71741b9eea591359867857fd2ae15aa535"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.4/xmux-v0.17.4-x86_64-apple-darwin.tar.gz"
      sha256 "5a653540ce226850686a5259449d55ed1ecf5305026afa8516734bfa343e33ea"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.4/xmux-v0.17.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a3cc029e92b4f81c409f0ac0ba8056152ff818a347ce22e9c32018a040a5b8ef"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.4/xmux-v0.17.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "24ca81d7f3a9944e466a1e0e1f3fbe82b7af9bc9b2367e3aea50e66bd4788793"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
