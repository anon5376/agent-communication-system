#!/bin/sh
# Build the aos terminal and install it as a global `aos` command.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
cargo build --release --manifest-path "$here/Cargo.toml" --bin aos
src="$here/target/release/aos"

install_to() {
    if [ -w "$1" ] || [ -w "$1/aos" ]; then
        cp "$src" "$1/aos"
        echo "installed: $1/aos"
        return 0
    fi
    if command -v sudo >/dev/null 2>&1; then
        sudo cp "$src" "$1/aos" && echo "installed: $1/aos" && return 0
    fi
    return 1
}

if install_to /usr/local/bin; then
    :
elif install_to "$HOME/.local/bin"; then
    case ":$PATH:" in
        *":$HOME/.local/bin:"*) ;;
        *) echo "note: add ~/.local/bin to your PATH (export PATH=\"\$HOME/.local/bin:\$PATH\")" ;;
    esac
else
    mkdir -p "$HOME/.local/bin"
    cp "$src" "$HOME/.local/bin/aos"
    echo "installed: $HOME/.local/bin/aos"
    echo "note: add ~/.local/bin to your PATH (export PATH=\"\$HOME/.local/bin:\$PATH\")"
fi

echo "try it:  aos demo     (opens a sample bus in a temp directory)"
echo "         aos          (uses ~/.agent-bus/bus.db, the same bus as qagent and acs)"
