#!/bin/bash
set -euo pipefail

# Build a .deb package from pre-compiled binaries.
# Usage: ./dist/build-deb.sh <target> <version>
# Example: ./dist/build-deb.sh x86_64-unknown-linux-gnu 0.1.3

TARGET="${1:?Usage: build-deb.sh <target> <version>}"
VERSION="${2:?Usage: build-deb.sh <target> <version>}"

case "$TARGET" in
    x86_64-unknown-linux-gnu)  ARCH="amd64" ;;
    aarch64-unknown-linux-gnu) ARCH="arm64" ;;
    *) echo "Unsupported target: $TARGET"; exit 1 ;;
esac

# Support both `cargo build --release` and `cargo build --release --target <target>`
if [ -d "target/${TARGET}/release" ]; then
    BINDIR="target/${TARGET}/release"
else
    BINDIR="target/release"
fi
PKG="tether_${VERSION}_${ARCH}"
ROOT="$PKG"

rm -rf "$ROOT"
mkdir -p "$ROOT/usr/local/bin"
mkdir -p "$ROOT/etc/systemd/system"
mkdir -p "$ROOT/DEBIAN"

cp "$BINDIR/tether"       "$ROOT/usr/local/bin/"
cp "$BINDIR/tetherd"      "$ROOT/usr/local/bin/"
cp "$BINDIR/tether-proxy" "$ROOT/usr/local/bin/"
cp dist/tetherd@.service  "$ROOT/etc/systemd/system/"

cat > "$ROOT/DEBIAN/control" <<EOF
Package: tether
Version: $VERSION
Architecture: $ARCH
Maintainer: hflsmax
Description: Persistent terminal sessions over SSH
 Survive disconnections, resume instantly. All traffic goes through SSH.
Depends: openssh-server
Priority: optional
Section: utils
EOF

cat > "$ROOT/DEBIAN/postinst" <<'EOF'
#!/bin/bash
systemctl daemon-reload
EOF
chmod 755 "$ROOT/DEBIAN/postinst"

dpkg-deb --build --root-owner-group "$ROOT"
echo "Built: ${PKG}.deb"
