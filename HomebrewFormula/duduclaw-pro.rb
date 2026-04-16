# Reference formula for homebrew-tap/Formula/duduclaw-pro.rb
# Copy this to the homebrew-tap repo and update version + sha256 per release.
#
# Since Pro is distributed as a pre-built tarball (not built from source),
# the formula downloads the tarball and extracts binary + Python SDK.
class DuduclawPro < Formula
  desc "DuDuClaw Pro — Multi-Agent AI Assistant Platform"
  homepage "https://github.com/zhixuli0406/DuDuClaw"
  version "1.4.29"
  license :cannot_represent

  on_arm do
    url "https://github.com/zhixuli0406/duduclaw-pro-releases/releases/download/v#{version}/duduclaw-pro-aarch64-apple-darwin.tar.gz"
    sha256 "REPLACE_WITH_ACTUAL_SHA256"
  end

  on_intel do
    url "https://github.com/zhixuli0406/duduclaw-pro-releases/releases/download/v#{version}/duduclaw-pro-x86_64-apple-darwin.tar.gz"
    sha256 "REPLACE_WITH_ACTUAL_SHA256"
  end

  depends_on :macos
  depends_on "python@3.12"

  def install
    if Hardware::CPU.arm?
      bin.install "duduclaw-pro-aarch64-apple-darwin" => "duduclaw-pro"
    else
      bin.install "duduclaw-pro-x86_64-apple-darwin" => "duduclaw-pro"
    end
    # Python SDK — required for evolution vetter & SDK chat fallback
    (libexec/"python").install "python/duduclaw"
  end

  def post_install
    ohai "Run `duduclaw-pro onboard` to set up your AI assistant"
  end

  test do
    assert_match "duduclaw", shell_output("#{bin}/duduclaw-pro version")
  end
end
