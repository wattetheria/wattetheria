#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust toolchain is required. Install rustup first: https://rustup.rs"
  exit 1
fi

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR"

cargo install --path apps/wattetheria-kernel --bin wattetheria-kernel --force
cargo install --path apps/wattetheria-cli --bin wattetheria-client-cli --force
cargo install --path apps/wattetheria-observatory --bin wattetheria-observatory --force

echo "Installed binaries: wattetheria-kernel, wattetheria-client-cli, wattetheria-observatory"
echo "Bootstrap a node:"
echo "  wattetheria-client-cli init --data-dir .wattetheria"
echo "  wattetheria-client-cli up --data-dir .wattetheria"
