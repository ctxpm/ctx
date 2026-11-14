#!/bin/sh
# Installs the ctx command-line tool from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/ctxpm/ctx/main/install.sh | sh
#
# Settings (environment variables):
#   CTX_VERSION      version to install, for example 0.1.0 (default: the latest release)
#   CTX_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
#   CTX_REPO         GitHub repository to download from (default: ctxpm/ctx)
set -eu

repo="${CTX_REPO:-ctxpm/ctx}"
version="${CTX_VERSION:-latest}"
bin_dir="${CTX_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
  echo "install.sh: $1" >&2
  exit 1
}

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin)
    target="universal-apple-darwin"
    ;;
  Linux)
    case "$arch" in
      x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
      *) fail "no prebuilt binary for Linux on $arch yet; build from source with: cargo install --git https://github.com/$repo ctx-cli" ;;
    esac
    ;;
  *)
    fail "no prebuilt binary for $os; on Windows, download ctx-x86_64-pc-windows-msvc.zip from https://github.com/$repo/releases"
    ;;
esac

asset="ctx-$target.tar.gz"
if [ "$version" = "latest" ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/cli-v${version#v}"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $base/$asset"
curl -fsSL "$base/$asset" -o "$tmp/$asset" || fail "download failed: $base/$asset"
curl -fsSL "$base/$asset.sha256" -o "$tmp/$asset.sha256" || fail "download failed: $base/$asset.sha256"

expected="$(cut -d ' ' -f 1 <"$tmp/$asset.sha256")"
if command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)"
elif command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)"
else
  fail "neither shasum nor sha256sum is available to verify the download"
fi
[ "$expected" = "$actual" ] || fail "checksum mismatch: expected $expected, got $actual; nothing was installed"

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$bin_dir"
mv "$tmp/ctx" "$bin_dir/ctx"
chmod +x "$bin_dir/ctx"

echo "Installed $("$bin_dir/ctx" --version) to $bin_dir/ctx"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "Add it to your PATH:  export PATH=\"$bin_dir:\$PATH\"" ;;
esac
