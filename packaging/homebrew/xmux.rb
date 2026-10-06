class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.15.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.15.0/xmux-v0.15.0-aarch64-apple-darwin.tar.gz"
      sha256 "6b4268f07dff6e4b0935ce97c7b55f7e2e276f177e1686dec261e763ed1b75a0"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.15.0/xmux-v0.15.0-x86_64-apple-darwin.tar.gz"
      sha256 "401ea6579d6d23301ef71dfdce8ce15c684882255cb19569b320f9af78e444e5"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.15.0/xmux-v0.15.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "6b6c69282c2f19dfb90be908fb6d818fca2a96f2428de6efaee18c8161ff3775"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.15.0/xmux-v0.15.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "12fa435dbe110b31fa54580308aef5133783102c24da4540b306a2f0fc9db0ae"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
