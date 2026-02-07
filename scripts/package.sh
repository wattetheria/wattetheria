#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DIST_DIR="$ROOT_DIR/dist"
mkdir -p "$DIST_DIR"

cd "$ROOT_DIR"
cargo build --release -p wattetheria-kernel -p wattetheria-client-cli -p wattetheria-observatory

OS_NAME="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH_NAME="$(uname -m)"
PKG_DIR="$DIST_DIR/wattetheria-${OS_NAME}-${ARCH_NAME}"
mkdir -p "$PKG_DIR/bin"

cp target/release/wattetheria-kernel "$PKG_DIR/bin/"
cp target/release/wattetheria-client-cli "$PKG_DIR/bin/"
cp target/release/wattetheria-observatory "$PKG_DIR/bin/"
cp README.md "$PKG_DIR/"

if command -v tar >/dev/null 2>&1; then
  tar -czf "$DIST_DIR/wattetheria-${OS_NAME}-${ARCH_NAME}.tar.gz" -C "$DIST_DIR" "wattetheria-${OS_NAME}-${ARCH_NAME}"
fi

echo "Package generated under $DIST_DIR"

if command -v zip >/dev/null 2>&1; then
  (cd "$DIST_DIR" && zip -rq "wattetheria-${OS_NAME}-${ARCH_NAME}.zip" "wattetheria-${OS_NAME}-${ARCH_NAME}")
fi
