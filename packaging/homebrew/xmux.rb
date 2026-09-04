class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.8.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.8.1/xmux-v0.8.1-aarch64-apple-darwin.tar.gz"
      sha256 "75bee71a2af44f009a73ba46d257af47584b06e2227d0da288d72c2fff0d07f6"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.8.1/xmux-v0.8.1-x86_64-apple-darwin.tar.gz"
      sha256 "06717fb24e811ff19395571d5698a5e565c9ae52023757f353c247e5712f4819"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.8.1/xmux-v0.8.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "b165e18f3975f4146e8972b8692045fff1a6334e79b94ef2f0e0f936a55ee634"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.8.1/xmux-v0.8.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "fa0419d1487a7cc962e6fee9a98d5c111083762873396ccf6389d209fd91f4ce"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
