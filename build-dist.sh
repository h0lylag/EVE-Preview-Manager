#!/usr/bin/env bash
# Build distribution for non-Nix Linux systems

set -e

echo "Building EVE Preview Manager distribution..."

# Build the project
nix build .#default

# Get version from Cargo.toml
VERSION=$(grep '^version = ' Cargo.toml | head -1 | cut -d'"' -f2)

# Create dist directory (separate from nix result/)
BUILD_DIR="dist"
DIST_DIR="$BUILD_DIR/eve-preview-manager-${VERSION}-x86_64"
rm -rf "$BUILD_DIR"
mkdir -p "$DIST_DIR"

echo "Copying binary..."

# Copy the unwrapped binary (the actual ELF executable)
if [ -f result/bin/.eve-preview-manager-wrapped ]; then
  cp result/bin/.eve-preview-manager-wrapped "$DIST_DIR/eve-preview-manager"
else
  cp result/bin/eve-preview-manager "$DIST_DIR/"
fi

# Make writable so we can patch it
chmod +w "$DIST_DIR/eve-preview-manager"

echo "Patching binary for system libraries..."

# Use patchelf to clear RPATH and set standard interpreter
nix shell nixpkgs#patchelf -c patchelf --remove-rpath "$DIST_DIR/eve-preview-manager"
nix shell nixpkgs#patchelf -c patchelf --set-interpreter /lib64/ld-linux-x86-64.so.2 "$DIST_DIR/eve-preview-manager" 2>/dev/null || \
nix shell nixpkgs#patchelf -c patchelf --set-interpreter /lib/x86_64-linux-gnu/ld-linux-x86-64.so.2 "$DIST_DIR/eve-preview-manager" 2>/dev/null || true

# Make it executable again (remove write to match original permissions)
chmod 755 "$DIST_DIR/eve-preview-manager"

echo ""
echo "✓ Distribution binary created: $DIST_DIR/eve-preview-manager"
echo ""
echo "Creating tarball for GitHub release..."
cd "$BUILD_DIR"
tar czf "eve-preview-manager-${VERSION}-x86_64.tar.gz" "eve-preview-manager-${VERSION}-x86_64"
echo "✓ Release tarball: $BUILD_DIR/eve-preview-manager-${VERSION}-x86_64.tar.gz"
