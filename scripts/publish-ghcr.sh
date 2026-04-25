#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

if [ -z "${RELEASE:-}" ]; then
  echo "RELEASE is required, for example: RELEASE=1.0.7 scripts/publish-ghcr.sh" >&2
  exit 1
fi

if [ -z "${GITHUB_TOKEN:-}" ]; then
  echo "GITHUB_TOKEN is required for private dependency fetches during Docker build" >&2
  exit 1
fi

scripts/verify-release-deployment.sh

docker buildx create --use --name watt-builder 2>/dev/null || docker buildx use watt-builder

docker buildx build \
  --platform linux/amd64,linux/arm64 \
  --secret id=github_token,env=GITHUB_TOKEN \
  --label "org.opencontainers.image.version=${RELEASE}" \
  -t "ghcr.io/wattetheria/wattetheria-kernel:${RELEASE}" \
  -t "ghcr.io/wattetheria/wattetheria-observatory:${RELEASE}" \
  --push \
  .
