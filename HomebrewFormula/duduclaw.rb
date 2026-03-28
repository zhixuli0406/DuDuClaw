class Duduclaw < Formula
  desc "Multi-Agent AI Assistant Platform"
  homepage "https://github.com/zhixuli0406/DuDuClaw"
  url "https://github.com/zhixuli0406/DuDuClaw.git", tag: "v0.8.6"
  version "0.8.6"
  license "MIT"

  head "https://github.com/zhixuli0406/DuDuClaw.git", branch: "main"

  depends_on "rust" => :build
  depends_on "node" => :build
  depends_on "python@3.12"

  def install
    cd "web" do
      system "npm", "ci", "--legacy-peer-deps"
      system "npm", "run", "build"
    end

    system "cargo", "build", "--release", "-p", "duduclaw-cli",
           "-p", "duduclaw-gateway", "--features", "duduclaw-gateway/dashboard"
    bin.install "target/release/duduclaw"
  end

  def post_install
    ohai "Run `duduclaw onboard` to set up your AI assistant"
  end

  test do
    assert_match "duduclaw", shell_output("#{bin}/duduclaw version")
  end
end
