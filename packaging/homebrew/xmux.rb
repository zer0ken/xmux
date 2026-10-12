class Xmux < Formula
  desc "A cross-machine, cross-mux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.20.7"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.7/xmux-v0.20.7-aarch64-apple-darwin.tar.gz"
      sha256 "bda5dd0100f3a384b4d14d0981461751a842f4786c15f0eb1bdd1544a8f20c18"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.7/xmux-v0.20.7-x86_64-apple-darwin.tar.gz"
      sha256 "b4c4e06f7a01b13deb28d9f34a8d9625b6ba1161845417645d3cd5a0c4d0a5e5"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.7/xmux-v0.20.7-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "987a3d0e08084fefca5c6a81b69bb082f0dfbe720bc431cfdddf78374a3f4287"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.20.7/xmux-v0.20.7-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "f638e6468c63b7364d2a4d33298e9f4507fa7b376d9f5fa4faa86fb87b5a175d"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
