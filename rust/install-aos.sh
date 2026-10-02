#!/bin/sh
# Build the aos terminal and install it as a global `aos` command.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
cargo build --release --manifest-path "$here/Cargo.toml" --bin aos
src="$here/target/release/aos"

install_to() {
    if [ -w "$1" ] || [ -w "$1/aos" ]; then
        # Rename over the old one so agents still running it are not disturbed.
        cp "$src" "$1/.aos.new" && mv -f "$1/.aos.new" "$1/aos"
        echo "installed: $1/aos"
        return 0
    fi
    if command -v sudo >/dev/null 2>&1; then
        sudo cp "$src" "$1/.aos.new" && sudo mv -f "$1/.aos.new" "$1/aos" && echo "installed: $1/aos" && return 0
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
    cp "$src" "$HOME/.local/bin/.aos.new" && mv -f "$HOME/.local/bin/.aos.new" "$HOME/.local/bin/aos"
    echo "installed: $HOME/.local/bin/aos"
    echo "note: add ~/.local/bin to your PATH (export PATH=\"\$HOME/.local/bin:\$PATH\")"
fi

echo "next: cd into a project folder and run  aos"
echo "      (to look around first with a sample team:  aos demo)"
