class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.11.5"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.5/xmux-v0.11.5-aarch64-apple-darwin.tar.gz"
      sha256 "146912d7ed832402e1d9d079a4cc2a2385cacb5e4974df97e18e9ab090153485"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.5/xmux-v0.11.5-x86_64-apple-darwin.tar.gz"
      sha256 "f88f2f2639aa63cc9f28bf0247bc21e9d73592fa94ffa3a1072e4db7b9365a45"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.5/xmux-v0.11.5-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "832f4d895a75f97d85552688b9eff004b455e6c8908ccee14e4593e89665eeac"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.11.5/xmux-v0.11.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "8273249e71db0d127c0bae35577472eb160ddd04e7ed7bb6dbb8ccf8535f9808"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
