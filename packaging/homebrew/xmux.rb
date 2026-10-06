class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.16.2"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.2/xmux-v0.16.2-aarch64-apple-darwin.tar.gz"
      sha256 "2b4ec0ea37aa7f48fabd407ae9891dad2032bf8f92f700345bec5a55a2f8e82d"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.2/xmux-v0.16.2-x86_64-apple-darwin.tar.gz"
      sha256 "4bfcb3bcc3eba19977e04179cf98266cc681fa0ed4172d46f4529b16ee7dd7df"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.2/xmux-v0.16.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "43b28eb24e88c6327e5aa924d0cb1a82508d58838c4a61167b8afcf138be30c3"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.16.2/xmux-v0.16.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5c9dfeb9db9aa6c7a55f725360cf5ae596741f3e4fbc629a19878ae3f8d3dff8"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
