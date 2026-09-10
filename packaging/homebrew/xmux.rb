class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.7"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.7/xmux-v0.9.7-aarch64-apple-darwin.tar.gz"
      sha256 "69eed367e36570d9167d7183e64f16cf4127539214e04dae9c7c7190f83c7bc2"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.7/xmux-v0.9.7-x86_64-apple-darwin.tar.gz"
      sha256 "d5eae33d8db6e658ae941d03bebd5a3874a440610081ea33f7253a8dfe2e5d78"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.7/xmux-v0.9.7-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "bf676cba3c7bda5a49e9ce7bf81c713322aeb3f647b889ca894986819343594e"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.7/xmux-v0.9.7-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "80893f48ebc7d55c32ecda936069aec35c3bbf084969ad7cce5b5c9c2087518e"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
