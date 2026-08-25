class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.6.0"

  on_arm do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.0/xmux-v0.6.0-aarch64-apple-darwin.tar.gz"
    sha256 "b7599bad781af114380d8386143f300e1614dc8fda726bacf40cbe7b13cf60d0"
  end

  on_intel do
    url "https://github.com/zer0ken/xmux/releases/download/v0.6.0/xmux-v0.6.0-x86_64-apple-darwin.tar.gz"
    sha256 "b0efa8ecfbe5c8ec3d09737c2098d31f8dcc494030f5e4b3d1a5bbd6094d9e0f"
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
