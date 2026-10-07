class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.17.5"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.5/xmux-v0.17.5-aarch64-apple-darwin.tar.gz"
      sha256 "4d719a2b721d36d7f497f7885cb9e5c2b9184a5950eb253230de220420f101d2"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.5/xmux-v0.17.5-x86_64-apple-darwin.tar.gz"
      sha256 "a23db6ffd01489162cc740062e92dfd23d0f743fca763560f51b484ddd265ef9"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.5/xmux-v0.17.5-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "404b3228aac4ef23e00cbe5224ac40ea0f283ee812ad322d83d9fb726aaaeb8e"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.17.5/xmux-v0.17.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "33cd254a641ab2f871c114c8951a3e875a02ccb1276c4f18362d122b85eda838"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
