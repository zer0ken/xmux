class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.20.1"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.1/xmux-v0.20.1-aarch64-apple-darwin.tar.gz"
      sha256 "23e473c183f269d299931ed5fb37a63f7e645175e9348c066459b49c365322fd"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.1/xmux-v0.20.1-x86_64-apple-darwin.tar.gz"
      sha256 "4f32121bffb9d3ba94257be924b52d6fc9147cf9ef8a24251d3880940f631c86"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.1/xmux-v0.20.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "159712cdd4f1a8377d5c8ef7b96d3529aae11022e45e9ac4b7cbaddb9f19451b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.1/xmux-v0.20.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "786d2302024d029255bb8594565a2674a3664e199b4bbed6466ad50d4240e06a"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
