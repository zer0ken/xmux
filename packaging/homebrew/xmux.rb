class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.19.0"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.19.0/xmux-v0.19.0-aarch64-apple-darwin.tar.gz"
      sha256 "bc0e110a8848b9191ad51188a26d4d8757a1d0cbd431fd170a38a201ccc22eb9"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.19.0/xmux-v0.19.0-x86_64-apple-darwin.tar.gz"
      sha256 "9b3d5ba7698b4e590cc072adf9159e616c46304941c58bfec40a3aceb2a12625"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.19.0/xmux-v0.19.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0eaa8e0f76c674e60814e4947d2f14e9e183ec35b4e0dc6e36a220e3c30feb91"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.19.0/xmux-v0.19.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0eff6285408e279d6a8ea86c1ed7d3938e3feb9db2b8feb12cef31782c583ab6"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
