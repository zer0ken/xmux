class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.9.17"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.17/xmux-v0.9.17-aarch64-apple-darwin.tar.gz"
      sha256 "88cd43245916258114bc99b33a6fec3a6bdce89294b84412cab3113beba6dccc"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.17/xmux-v0.9.17-x86_64-apple-darwin.tar.gz"
      sha256 "2739f46345a42d00edf6a3ca22d297ba298335e1be4a105fe0a7c898a2b48e8a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.17/xmux-v0.9.17-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "44bc74336d799c55a8df88523630703304516c13a1e86ff907df4bf0b371a724"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.9.17/xmux-v0.9.17-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "1808d5f9e9ccaf4e09d51761da938720997f12b4337b131602e746496afe23d2"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
