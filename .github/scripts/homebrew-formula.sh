#!/usr/bin/env bash
# Prints the Homebrew formula for a release: homebrew-formula.sh <version without v> <SHA256SUMS.txt>
set -euo pipefail

version="$1"
sums="$2"
base="https://github.com/z-x/github-pr-check/releases/download/v${version}"

asset() {
  local file="github-pr-check-v${version}-$1.tar.gz"
  local sha
  sha=$(awk -v f="$file" '$2 == f { print $1 }' "$sums")
  [ -n "$sha" ] || { echo "no checksum for $file" >&2; exit 1; }
  printf '      url "%s/%s"\n      sha256 "%s"\n' "$base" "$file" "$sha"
}

cat <<EOF
class GithubPrCheck < Formula
  desc "Terminal dashboard showing live CI status of your open GitHub pull requests"
  homepage "https://github.com/z-x/github-pr-check"
  version "${version}"
  license "MIT"

  on_macos do
    on_arm do
$(asset aarch64-apple-darwin)
    end
    on_intel do
$(asset x86_64-apple-darwin)
    end
  end

  on_linux do
    on_arm do
$(asset aarch64-unknown-linux-musl)
    end
    on_intel do
$(asset x86_64-unknown-linux-musl)
    end
  end

  def install
    bin.install "github-pr-check"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/github-pr-check --version")
  end
end
EOF
