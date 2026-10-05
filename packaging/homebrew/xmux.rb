class Xmux < Formula
  desc "Cross-environment tmux/psmux session switcher"
  homepage "https://github.com/zer0ken/xmux"
  license "MIT"
  version "0.12.5"

  on_macos do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.5/xmux-v0.12.5-aarch64-apple-darwin.tar.gz"
      sha256 "b9890a003911b25f4db8480d5c45840da7670798429e3d5064aa1fa8c4eeb856"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.5/xmux-v0.12.5-x86_64-apple-darwin.tar.gz"
      sha256 "6c743e6309b8754a625c83ab919cfcae15198f40a88a4a6964c5fcf15c120d57"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.5/xmux-v0.12.5-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5eed309c80cefaaca89d4db4a2e3dcfe35ce8b04d5917ca1a822fd504bf8045c"
    end

    on_intel do
      url "https://github.com/zer0ken/xmux/releases/download/v0.12.5/xmux-v0.12.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "afe0cec9a70da752bc148693bd0388c35015b147a57a8ea974272957c1584436"
    end
  end

  def install
    bin.install "xmux"
  end

  test do
    system "#{bin}/xmux", "version"
  end
end
