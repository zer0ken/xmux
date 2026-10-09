class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.18.4"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.4/xmux-v0.18.4-aarch64-apple-darwin.tar.gz"
      sha256 "b53b37a84aaed00fc7a5ea0bbaa60251d159d25be31678274c27a93cbca91cc7"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.4/xmux-v0.18.4-x86_64-apple-darwin.tar.gz"
      sha256 "99aa8bc0c04e28e293f035c563335455cae8c31ca023faeb8598749a60cdd227"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.4/xmux-v0.18.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "85adafd61c377d2043c93472485114fdd45e727d5bef5dc304af41fa1d06560b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.18.4/xmux-v0.18.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b0e457829517e56f8a603400e31ba99df42e9de893905d6f5791bebe4f3b2bca"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
