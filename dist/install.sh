#!/bin/bash
set -euo pipefail

# Install tether server binaries and systemd service on Linux (Ubuntu, Debian, etc.)

INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
REPO="hflsmax/tether"

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
    x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
    aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
    *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
esac

echo "Installing tether server ($TARGET) to $INSTALL_DIR..."

# Download binaries
for bin in tetherd tether-proxy; do
    echo "  Downloading $bin..."
    curl -fSL "https://github.com/$REPO/releases/latest/download/$bin-$TARGET" -o "$INSTALL_DIR/$bin"
    chmod +x "$INSTALL_DIR/$bin"
done

# Install systemd service template
echo "  Installing systemd service..."
curl -fSL "https://github.com/$REPO/releases/latest/download/tetherd@.service" -o /etc/systemd/system/tetherd@.service
systemctl daemon-reload

echo ""
echo "Done. To enable tether for a user:"
echo ""
echo "  sudo systemctl enable --now tetherd@\$USER"
echo ""
echo "Make sure tether-proxy is in the user's PATH (it's in $INSTALL_DIR)."
