# Homebrew Formula for uf (URL finder)
#
# This file lives in the tap repo: https://github.com/HaomingJu/homebrew-tap
# Update sha256 values after each release by running:
#   shasum -a 256 uf-<version>-<target>.tar.gz
#
class Uf < Formula
  desc "Terminal fuzzy search launcher for browser history, bookmarks, GitHub, GitLab, and DockerHub"
  homepage "https://github.com/HaomingJu/uf"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/HaomingJu/uf/releases/download/v#{version}/uf-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_AARCH64_APPLE_DARWIN"
    end
    on_intel do
      url "https://github.com/HaomingJu/uf/releases/download/v#{version}/uf-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_X86_64_APPLE_DARWIN"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/HaomingJu/uf/releases/download/v#{version}/uf-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER_AARCH64_LINUX"
    end
    on_intel do
      url "https://github.com/HaomingJu/uf/releases/download/v#{version}/uf-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER_X86_64_LINUX"
    end
  end

  depends_on "sqlite"

  def install
    bin.install "uf"
  end

  test do
    assert_match "uf", shell_output("#{bin}/uf --help", 0)
  end
end
