class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.16.7"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.7/xmux-v0.16.7-aarch64-apple-darwin.tar.gz"
      sha256 "3655b2a71263eb790905972aa0ef56a946d44489286015860559163faaf7c8f9"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.7/xmux-v0.16.7-x86_64-apple-darwin.tar.gz"
      sha256 "fcafc555a6e82c669d0488f4414a3f21e8f8816a80305e9c2fdc9ac2264617d8"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.7/xmux-v0.16.7-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "71f6f5e68b11a586d734b5e55b268ff6e03e5828f08dccdfc16715120a07f67b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.7/xmux-v0.16.7-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "4bba172fb84ddb20d50f091127b42a051c42d32e3d38c0e02ddbb09c4571d002"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
