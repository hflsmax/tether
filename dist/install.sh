#!/bin/bash
set -euo pipefail

# Install tether server binaries and systemd service on Linux (Ubuntu, Debian, etc.)
#
# Usage:
#   sudo ./install.sh          # System-wide install (requires root)
#   ./install.sh --user        # Per-user install (no root needed, requires loginctl enable-linger)

REPO="hflsmax/tether"
RELEASE_URL="${RELEASE_URL:-https://github.com/$REPO/releases/latest/download}"
USER_MODE=0

for arg in "$@"; do
    case "$arg" in
        --user) USER_MODE=1 ;;
        *) echo "Unknown option: $arg"; exit 1 ;;
    esac
done

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
    x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
    aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
    *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
esac

if [ "$USER_MODE" -eq 1 ]; then
    INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
    mkdir -p "$INSTALL_DIR"

    echo "Installing tether server ($TARGET) to $INSTALL_DIR (user mode)..."

    # Download binaries
    for bin in tetherd tether-proxy; do
        echo "  Downloading $bin..."
        curl -fSL "$RELEASE_URL/$bin-$TARGET" -o "$INSTALL_DIR/$bin"
        chmod +x "$INSTALL_DIR/$bin"
    done

    # Install systemd user service
    UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
    mkdir -p "$UNIT_DIR"
    echo "  Installing systemd user service to $UNIT_DIR..."
    curl -fSL "$RELEASE_URL/tetherd.user.service" -o "$UNIT_DIR/tetherd.service"

    # Patch ExecStart if installed to non-default location
    if [ "$INSTALL_DIR" != "/usr/local/bin" ]; then
        sed -i "s|ExecStart=/usr/local/bin/tetherd|ExecStart=$INSTALL_DIR/tetherd|" "$UNIT_DIR/tetherd.service"
    fi

    systemctl --user daemon-reload

    # Check if INSTALL_DIR is in PATH
    if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
        SHELL_NAME="$(basename "$SHELL")"
        case "$SHELL_NAME" in
            zsh)  RC="$HOME/.zshrc"
                  PATH_LINE="export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
            fish) RC="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
                  PATH_LINE="fish_add_path $INSTALL_DIR" ;;
            *)    RC="$HOME/.bashrc"
                  PATH_LINE="export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
        esac
        echo ""
        echo "WARNING: $INSTALL_DIR is not in your PATH."
        echo "Add it by running:"
        echo ""
        echo "  echo '$PATH_LINE' >> $RC"
        echo ""
    fi

    echo ""
    echo "Done. To start tether:"
    echo ""
    echo "  systemctl --user enable --now tetherd"
    echo ""
    echo "To survive logout (run once):"
    echo ""
    echo "  sudo loginctl enable-linger \$USER"
else
    INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

    echo "Installing tether server ($TARGET) to $INSTALL_DIR..."

    # Download binaries
    for bin in tetherd tether-proxy; do
        echo "  Downloading $bin..."
        curl -fSL "$RELEASE_URL/$bin-$TARGET" -o "$INSTALL_DIR/$bin"
        chmod +x "$INSTALL_DIR/$bin"
    done

    # Install systemd service template
    echo "  Installing systemd service..."
    curl -fSL "$RELEASE_URL/tetherd@.service" -o /etc/systemd/system/tetherd@.service
    systemctl daemon-reload

    echo ""
    echo "Done. To enable tether for a user:"
    echo ""
    echo "  sudo systemctl enable --now tetherd@\$USER"
    echo ""
    echo "Make sure tether-proxy is in the user's PATH (it's in $INSTALL_DIR)."
fi
