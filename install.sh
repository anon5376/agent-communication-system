#!/bin/sh
# Install aos, mission control for a team of AI coding agents.
#
#   curl -fsSL https://raw.githubusercontent.com/anon5376/agent-communication-system/rust-port/install.sh | sh
#
# It downloads a prebuilt aos for your computer from the project's GitHub
# releases. If there is none yet, it builds aos from source, which needs Git,
# Rust (rustup.rs) and a C compiler. Nothing else is installed or changed.
#
# Settings (environment variables):
#   AOS_INSTALL_DIR   where the aos command goes (default ~/.local/bin)
#   AOS_FROM_SOURCE=1 skip the download and build from source
#   AOS_BRANCH        the branch to build from source (default rust-port)
set -eu

repo="anon5376/agent-communication-system"
branch="${AOS_BRANCH:-rust-port}"
dest="${AOS_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
fail() { printf 'aos install: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

case "$(uname -s)" in
    Linux) os=unknown-linux-musl ;;
    Darwin) os=apple-darwin ;;
    *) fail "aos runs on Linux and macOS. On Windows, use WSL2 and run this there." ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) fail "no aos build for $(uname -m) yet." ;;
esac
target="$arch-$os"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fetch() {
    if have curl; then curl -fsSL "$1" -o "$2"
    elif have wget; then wget -q "$1" -O "$2"
    else return 1
    fi
}

# Replace the command without disturbing agents still running the old one:
# write next to it, then rename over it.
put() {
    mkdir -p "$dest" || fail "cannot create $dest; set AOS_INSTALL_DIR to a folder you can write"
    cp "$1" "$dest/.aos.new"
    chmod 755 "$dest/.aos.new"
    mv -f "$dest/.aos.new" "$dest/aos"
}

download() {
    # The newest release that carries aos builds (tags start with aos-v).
    fetch "https://api.github.com/repos/$repo/releases?per_page=30" "$tmp/releases.json" 2>/dev/null || return 1
    url="$(grep -o "https://github.com/$repo/releases/download/aos-v[^\"]*/aos-$target.tar.gz" "$tmp/releases.json" | head -n 1)"
    [ -n "$url" ] || return 1
    say "downloading $url"
    fetch "$url" "$tmp/aos.tar.gz" || return 1
    if fetch "$url.sha256" "$tmp/aos.tar.gz.sha256" 2>/dev/null; then
        want="$(cut -d ' ' -f 1 "$tmp/aos.tar.gz.sha256")"
        if have sha256sum; then got="$(sha256sum "$tmp/aos.tar.gz" | cut -d ' ' -f 1)"
        elif have shasum; then got="$(shasum -a 256 "$tmp/aos.tar.gz" | cut -d ' ' -f 1)"
        else got="$want"
        fi
        [ "$want" = "$got" ] || fail "checksum mismatch for $url; nothing was installed"
    fi
    tar -xzf "$tmp/aos.tar.gz" -C "$tmp" aos || return 1
    put "$tmp/aos"
}

build() {
    have git || fail "building aos needs Git. Install it, then run this again."
    if ! have cargo; then
        say ""
        say "There is no ready-made aos for your computer yet, so it has to be built,"
        say "and building needs Rust. Install Rust (about 2 minutes), then run this again:"
        say ""
        say "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        say ""
        exit 1
    fi
    have cc || have gcc || have clang || fail "building aos needs a C compiler: on macOS run xcode-select --install; on Debian or Ubuntu, sudo apt install build-essential."
    say "building aos from source ($branch); this takes a few minutes the first time"
    git clone --quiet --depth 1 --branch "$branch" "https://github.com/$repo.git" "$tmp/src"
    cargo build --quiet --release --locked --manifest-path "$tmp/src/rust/Cargo.toml" --bin aos
    put "$tmp/src/rust/target/release/aos"
}

if [ "${AOS_FROM_SOURCE:-}" = "1" ] || ! download; then
    build
fi

say ""
say "installed $dest/aos ($("$dest/aos" --version 2>/dev/null || echo aos))"
case ":$PATH:" in
    *":$dest:"*) ;;
    *)
        say ""
        case "${SHELL:-}" in
            *zsh) rc="~/.zshrc" ;;
            *bash) rc="~/.bashrc" ;;
            *) rc="~/.profile" ;;
        esac
        say "$dest is not on your PATH yet. Add it, then open a new terminal:"
        say "    echo 'export PATH=\"$dest:\$PATH\"' >> $rc"
        ;;
esac
say ""
say "next: cd into a project folder and run  aos"
say "      (to look around first with a simulated team, no account needed:  aos demo)"
