#!/bin/sh
# Print the Homebrew formula for a published release, using its checksum files.
# Usage: scripts/homebrew-formula.sh v1.8.0 > Formula/gd.rb   (needs gh)
set -eu
tag=$1

base="https://github.com/grovedock/gd/releases/download/$tag"
sum() {
  gh release download "$tag" --repo grovedock/gd --pattern "gd-$tag-$1.tar.gz.sha256" --output - | cut -d' ' -f1
}
# Fetch every checksum before printing anything: a release still being built has no assets
# yet, and a formula with an empty checksum must never be written.
for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  s=$(sum "$t") || s=""
  case "$s" in
    [0-9a-f]*) ;;
    *) echo "no checksum for $t in $tag; is the release published?" >&2; exit 1 ;;
  esac
  eval "sum_$(echo "$t" | tr '-' '_')=$s"
done
asset() {
  eval "s=\$sum_$(echo "$1" | tr '-' '_')"
  printf '      url "%s/gd-%s-%s.tar.gz"\n      sha256 "%s"\n' "$base" "$tag" "$1" "$s"
}
cat <<RUBY
class Grovedock < Formula
  desc "Source control for monorepos at agent scale"
  homepage "https://grovedock.com/"
  license "Apache-2.0"

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
$(asset aarch64-unknown-linux-gnu)
    end
    on_intel do
$(asset x86_64-unknown-linux-gnu)
    end
  end

  def install
    bin.install "gd", "grovedockd", "git-remote-gd"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/gd --version")
    assert_match version.to_s, shell_output("#{bin}/grovedockd --version")
  end
end
RUBY
