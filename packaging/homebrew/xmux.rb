class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.11.2"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.2/xmux-v0.11.2-aarch64-apple-darwin.tar.gz"
      sha256 "2a08744d85c4959435f10a27aa1025671eede239ca317bdb4813f2efde7517d0"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.2/xmux-v0.11.2-x86_64-apple-darwin.tar.gz"
      sha256 "921d20fa41bfcb64fe0f6eaffbad75182a4972a5ca63a4e1d0b5835d12e03f9e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.2/xmux-v0.11.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "8de54c12e84e4e38230356250674fce22b2adc77a9243ea199777e9d9154eb8b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.2/xmux-v0.11.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b181a777e0361628168d91d46441e39a74fe2ab1a6f2ca56328c8fc1faa8962c"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
