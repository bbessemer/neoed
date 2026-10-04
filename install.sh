#!/bin/sh
# Installs the latest ned release:
#   curl -fsSL https://raw.githubusercontent.com/bbessemer/neoed/main/install.sh | sh
#
# Environment:
#   NED_VERSION      release tag to install, e.g. v0.7.0 (default: latest)
#   NED_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
set -eu

repo=bbessemer/neoed
install_dir=${NED_INSTALL_DIR:-$HOME/.local/bin}
from_source="build from source: cargo install --locked --git https://github.com/$repo ned-cli"

err() {
    echo "ned install: $*" >&2
    exit 1
}

case $(uname -s) in
    Linux) os=unknown-linux-musl ;;
    Darwin) os=apple-darwin ;;
    *) err "no release for $(uname -s); $from_source" ;;
esac

case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) err "no release for $(uname -m); $from_source" ;;
esac

# An x86_64 shell under Rosetta still runs on Apple silicon; install the native binary.
if [ "$os" = apple-darwin ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = 1 ]; then
    arch=aarch64
fi

target=$arch-$os
if [ -n "${NED_VERSION:-}" ]; then
    url=https://github.com/$repo/releases/download/$NED_VERSION/ned-$target.tar.gz
else
    url=https://github.com/$repo/releases/latest/download/ned-$target.tar.gz
fi

if command -v sha256sum >/dev/null; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    err "need sha256sum or shasum to verify the download"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $url"
curl -fsSL "$url" -o "$tmp/ned.tar.gz" || err "download failed: $url"
curl -fsSL "$url.sha256" -o "$tmp/ned.tar.gz.sha256" || err "download failed: $url.sha256"
[ "$(sha256 "$tmp/ned.tar.gz")" = "$(cut -d' ' -f1 "$tmp/ned.tar.gz.sha256")" ] ||
    err "checksum mismatch for $url"

tar xzf "$tmp/ned.tar.gz" -C "$tmp"
mkdir -p "$install_dir"
mv "$tmp/ned-$target/ned" "$install_dir/ned"
echo "Installed $("$install_dir/ned" --version) to $install_dir/ned"

case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) echo "Add $install_dir to your PATH to run ned." ;;
esac
