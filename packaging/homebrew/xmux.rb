class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.20.6"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.6/xmux-v0.20.6-aarch64-apple-darwin.tar.gz"
      sha256 "33c8f90698a0ebfb126dbafb6daa41e0488c59193657c02fe2b2880dc6e753af"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.6/xmux-v0.20.6-x86_64-apple-darwin.tar.gz"
      sha256 "0ba17bf42d9a6a854e9b19852d890f8e5ea8568c5d92beef578231eef57fe034"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.6/xmux-v0.20.6-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "37974e7b2e6484b660b7051ade2cb308323365d48f0bb04cfb5ad3d5d865502c"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.6/xmux-v0.20.6-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "9ba3bc2f15315cb4cf16aaef39020643f27f45cbea1092a9215082f32fd34827"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
