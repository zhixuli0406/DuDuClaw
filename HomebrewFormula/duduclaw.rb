class Duduclaw < Formula
  desc "Multi-Agent AI Assistant Platform"
  homepage "https://github.com/zhixuli0406/DuDuClaw"
  version "0.1.0"
  license "MIT"

  # For now, build from source.
  # When releases are available, add bottle blocks for pre-built binaries.

  on_macos do
    on_arm do
      url "https://github.com/zhixuli0406/DuDuClaw.git", tag: "v0.1.0"
    end
    on_intel do
      url "https://github.com/zhixuli0406/DuDuClaw.git", tag: "v0.1.0"
    end
  end
  on_linux do
    url "https://github.com/zhixuli0406/DuDuClaw.git", tag: "v0.1.0"
  end

  depends_on "rust" => :build
  depends_on "node" => :build # for dashboard
  depends_on "python@3.12"

  def install
    # Build frontend
    cd "web" do
      system "npm", "ci", "--legacy-peer-deps"
      system "npm", "run", "build"
    end

    # Build Rust binary with dashboard
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
