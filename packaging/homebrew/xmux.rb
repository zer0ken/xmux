class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.12.2"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.2/xmux-v0.12.2-aarch64-apple-darwin.tar.gz"
      sha256 "f9965d4f9f93f1aae4165bd13508161329e1e7b928be542b679f8c3d82ceda5b"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.2/xmux-v0.12.2-x86_64-apple-darwin.tar.gz"
      sha256 "4951445ff34de3755cd37a4f960348c3ea3c644d696254a7a3bb58eb41be5622"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.2/xmux-v0.12.2-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "61863fd564726e2f564b3f8e393f277da15e737cd114302500725b83645ef206"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.2/xmux-v0.12.2-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "8b8da26e0e498f6a22121b8d171dd3da3251784dbde9a6fce8c93e45ec0069b9"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
