# Homebrew formula for tael.
#
# This file is the source of truth; the published tap
# (thousandbirdsinc/homebrew-tael) carries a copy that the release workflow
# updates with each version's URLs and checksums. Keeping the formula in this
# repo means a change to the install surface is reviewed alongside the change
# that caused it.
#
# Install:
#   brew tap thousandbirdsinc/tael
#   brew install tael
class Tael < Formula
  desc "AI-agent-native observability platform: OTLP ingest, tiered storage, CLI-first queries"
  homepage "https://github.com/thousandbirdsinc/tael"
  version "0.5.14"
  license "MIT"

  # Prebuilt binaries from the GitHub Release. Building from source would pull
  # a large dependency tree for no benefit, since the release artifacts are
  # produced from this same tag.
  on_macos do
    on_arm do
      url "https://github.com/thousandbirdsinc/tael/releases/download/v#{version}/tael-cli-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_RELEASE_SHA256"
    end
    on_intel do
      url "https://github.com/thousandbirdsinc/tael/releases/download/v#{version}/tael-cli-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_RELEASE_SHA256"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/thousandbirdsinc/tael/releases/download/v#{version}/tael-cli-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_RELEASE_SHA256"
    end
    on_intel do
      url "https://github.com/thousandbirdsinc/tael/releases/download/v#{version}/tael-cli-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_RELEASE_SHA256"
    end
  end

  def install
    bin.install Dir["**/tael"].first => "tael"
  end

  def caveats
    <<~EOS
      Start the server:
        tael serve

      It listens on 127.0.0.1 only, with no authentication — reachability is
      authorization. Binding a non-loopback address requires API keys:
        tael auth create-key --name my-agent --role writer

      Install the Claude Code skill so an agent knows how to drive it:
        tael skill install
    EOS
  end

  service do
    run [opt_bin/"tael", "serve"]
    keep_alive true
    log_path var/"log/tael.log"
    error_log_path var/"log/tael.log"
  end

  test do
    assert_match "tael", shell_output("#{bin}/tael --help")
    # `server status` always exits 0 by design and reports reachability in its
    # JSON, so the test asserts on the payload rather than the exit code.
    assert_match "unreachable", shell_output("#{bin}/tael --format json server status")
  end
end
