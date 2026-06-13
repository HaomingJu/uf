class Uf < Formula
  desc "Terminal fuzzy search launcher for browser history, bookmarks, GitHub, GitLab, and DockerHub"
  homepage "https://github.com/HaomingJu/uf"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/HaomingJu/uf/releases/download/v#{version}/uf-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "e322123cbe62034bb6745e81625fcd5971eaeecf16326390f4da538a5fe4dbca"
    end
    on_intel do
      url "https://github.com/HaomingJu/uf/releases/download/v#{version}/uf-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "ef0861e46c21d5fc7cfaffaab81854b6e835f37cb4a4cbbd6932e1c371142633"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/HaomingJu/uf/releases/download/v#{version}/uf-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "1d234db85d0420e313a4dddd264622706775039d32aa45fcea925d9b293f91dd"
    end
    on_intel do
      url "https://github.com/HaomingJu/uf/releases/download/v#{version}/uf-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b682eccfd5849111c483598ffdf1eb6b408e6737e471402f21a4b2d8a16c15f2"
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
