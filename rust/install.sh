#!/bin/sh
# Build the acs TUI and install it as a global `acs` command.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
cargo build --release --manifest-path "$here/Cargo.toml" --bin acs
src="$here/target/release/acs"

install_to() {
    if [ -w "$1" ] || [ -w "$1/acs" ]; then
        cp "$src" "$1/acs"
        echo "installed: $1/acs"
        return 0
    fi
    if command -v sudo >/dev/null 2>&1; then
        sudo cp "$src" "$1/acs" && echo "installed: $1/acs" && return 0
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
    cp "$src" "$HOME/.local/bin/acs"
    echo "installed: $HOME/.local/bin/acs"
    echo "note: add ~/.local/bin to your PATH (export PATH=\"\$HOME/.local/bin:\$PATH\")"
fi

echo "run it: acs          (uses ~/.agent-bus/bus.db)"
echo "        acs --db /path/to/bus.db"
