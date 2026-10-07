class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.16.4"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.4/xmux-v0.16.4-aarch64-apple-darwin.tar.gz"
      sha256 "81c674afedc12168e4ec8b64f9c3590cb81051334ef858af897564b77370422a"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.4/xmux-v0.16.4-x86_64-apple-darwin.tar.gz"
      sha256 "fc4592fe25411078992c362c1623d59db7ad2f1bf0cdec87325ce35e58bc36cf"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.4/xmux-v0.16.4-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0948a29f33119789470d714270abe6fb2978d626d5cfa7646608e489def6c71c"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.4/xmux-v0.16.4-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e2d4ca3928c3f9510d687d129cd6ff461af2f3b336f11d7607035b63f3a73ecc"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
