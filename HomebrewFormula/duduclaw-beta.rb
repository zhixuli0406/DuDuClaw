class DuduclawBeta < Formula
  desc "Multi-Agent AI Assistant Platform (beta: prediction hardening + embedding)"
  homepage "https://github.com/zhixuli0406/DuDuClaw"
  url "https://github.com/zhixuli0406/DuDuClaw.git", branch: "release/v1.2.0-beta.1"
  version "1.2.0-beta.1"
  license "Elastic-2.0"

  head "https://github.com/zhixuli0406/DuDuClaw.git", branch: "release/v1.2.0-beta.1"

  depends_on "rust" => :build
  depends_on "node" => :build
  depends_on "python@3.12"

  # Conflicts with stable duduclaw — only one can be installed at a time
  conflicts_with "duduclaw", because: "both install a `duduclaw` binary"

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
    ohai "DuDuClaw v1.2.0-beta.1 installed"
    ohai "Changes: prediction hardening, embedding integration, evolution logging"
    ohai "Run `duduclaw onboard` to set up, or `duduclaw serve` to start"
  end

  def caveats
    <<~EOS
      This is a BETA release for testing the prediction engine hardening.
      It includes: FeedbackSeverity grading, vocabulary_novelty fallback,
      evolution event logging, epsilon-floor exploration, and anti-sycophancy.

      To switch back to stable:
        brew uninstall duduclaw-beta
        brew install duduclaw
    EOS
  end

  test do
    assert_match "duduclaw", shell_output("#{bin}/duduclaw version")
  end
end
