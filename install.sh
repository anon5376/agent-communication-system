#!/bin/sh
# Install four ACS commands from one tagged, checksummed release.
# ACS_VERSION=v1.0.0 ACS_INSTALL_DIR=$HOME/.local/bin sh install.sh
# Explicit source build: ACS_FROM_SOURCE=1 ACS_VERSION=<tag> sh install.sh
set -eu
repo=anon5376/agent-communication-system
dest=${ACS_INSTALL_DIR:-${AOS_INSTALL_DIR:-$HOME/.local/bin}}
version=${ACS_VERSION:-}
source_build=${ACS_FROM_SOURCE:-${AOS_FROM_SOURCE:-0}}
commands='acs aos qagent acs-desktop'
fail() { printf 'ACS install: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
fetch() {
    if have curl; then curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2"
    elif have wget; then wget --https-only -q "$1" -O "$2"
    else fail 'install curl or wget first'; fi
}
case "$(uname -s)" in
    Linux) os=unknown-linux-musl ;;
    Darwin) os=apple-darwin ;;
    *) fail 'Use the Windows setup.exe from https://github.com/anon5376/agent-communication-system/releases' ;;
esac
case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    arm64|aarch64) arch=aarch64 ;;
    *) fail "unsupported architecture: $(uname -m)" ;;
esac
target=$arch-$os
tmp=$(mktemp -d)
stage=
trap 'rm -rf "$tmp"; if [ -n "$stage" ]; then rm -rf "$stage"; fi' 0
trap 'exit 130' INT
trap 'exit 143' TERM
if [ "$source_build" = 1 ] && [ -z "$version" ]; then
    fail 'source builds require ACS_VERSION=<tag>; no branch is selected implicitly'
fi
if [ -z "$version" ]; then
    fetch "https://api.github.com/repos/$repo/releases/latest" "$tmp/release.json" || fail 'cannot resolve latest release; set ACS_VERSION=<tag> to pin a release'
    version=$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$tmp/release.json")
fi
case "$version" in ''|*[!a-zA-Z0-9._-]*) fail 'invalid release tag' ;; esac
printf 'ACS release: %s\nTarget: %s\nDestination: %s\n' "$version" "$target" "$dest"
if [ "$source_build" = 1 ]; then
    have git && have cargo || fail 'explicit source builds require Git, Rust and a C compiler'
    printf 'Source: https://github.com/%s.git at tag %s (not a prebuilt release)\n' "$repo" "$version"
    git init -q "$tmp/src"
    git -C "$tmp/src" remote add origin "https://github.com/$repo.git"
    git -C "$tmp/src" fetch -q --depth 1 origin "refs/tags/$version" || fail 'requested tag does not exist'
    git -C "$tmp/src" checkout -q --detach FETCH_HEAD
    cargo build --release --locked --manifest-path "$tmp/src/rust/Cargo.toml" --bin acs --bin aos --bin qagent --bin acs-desktop
    binaries=$tmp/src/rust/target/release
else
    asset=acs-$target.tar.gz
    url=https://github.com/$repo/releases/download/$version/$asset
    printf 'Source: %s\n' "$url"
    fetch "$url" "$tmp/$asset" || fail 'this tag has no ACS archive for your platform; nothing installed (source builds require ACS_FROM_SOURCE=1)'
    fetch "$url.sha256" "$tmp/checksum" || fail 'checksum is missing; refusing to install'
    want=$(awk 'NR==1 {print $1}' "$tmp/checksum")
    [ "${#want}" = 64 ] || fail 'invalid checksum'
    case "$want" in *[!a-fA-F0-9]*) fail 'invalid checksum' ;; esac
    if have sha256sum; then got=$(sha256sum "$tmp/$asset" | awk '{print $1}')
    elif have shasum; then got=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
    else fail 'SHA-256 verification requires sha256sum or shasum'; fi
    [ "$(printf '%s' "$want" | tr A-F a-f)" = "$got" ] || fail 'checksum mismatch; nothing installed'
    printf 'SHA-256 verified: %s\n' "$got"
    tar -tzf "$tmp/$asset" > "$tmp/entries" || fail 'invalid archive'
    printf '%s\n' acs acs-desktop aos qagent > "$tmp/expected"
    LC_ALL=C sort "$tmp/entries" > "$tmp/sorted"
    cmp -s "$tmp/expected" "$tmp/sorted" || fail 'unexpected archive contents'
    mkdir "$tmp/bin"
    tar -xzf "$tmp/$asset" -C "$tmp/bin"
    binaries=$tmp/bin
fi
mkdir -p "$dest" || fail "cannot create $dest"
stage=$(mktemp -d "$dest/.acs-install.XXXXXX")
for name in $commands; do
    [ -f "$binaries/$name" ] && [ ! -L "$binaries/$name" ] || fail "missing regular binary: $name"
    cp "$binaries/$name" "$stage/$name"
    chmod 755 "$stage/$name"
done
for name in $commands; do mv -f "$stage/$name" "$dest/$name"; done
printf '\nInstalled acs, aos, qagent and acs-desktop from %s.\n' "$version"
case ":$PATH:" in *":$dest:"*) ;; *) printf 'Add this directory to PATH: %s\n' "$dest" ;; esac
printf 'Try: aos demo\nSet up real tools: aos setup\nDesktop downloads: https://github.com/%s/releases/tag/%s\n' "$repo" "$version"
