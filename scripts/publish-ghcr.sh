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

IMAGES=(
  ghcr.io/wattetheria/wattetheria-kernel
)

# PLATFORMS narrows the build to one architecture (for example on a native runner).
# PUSH_BY_DIGEST=1 pushes untagged per-architecture images and writes the digest to
# METADATA_FILE, so CI can merge the architectures into one multi-arch tag afterwards.
build_args=(
  --platform "${PLATFORMS:-linux/amd64,linux/arm64}"
  --secret id=github_token,env=GITHUB_TOKEN
  --label "org.opencontainers.image.version=${RELEASE}"
)

if [ "${PUSH_BY_DIGEST:-0}" = "1" ]; then
  if [ -z "${METADATA_FILE:-}" ]; then
    echo "METADATA_FILE is required when PUSH_BY_DIGEST=1" >&2
    exit 1
  fi
  image_names="$(IFS=,; echo "${IMAGES[*]}")"
  build_args+=(
    --output "type=image,\"name=${image_names}\",push-by-digest=true,name-canonical=true,push=true"
    --metadata-file "$METADATA_FILE"
  )
else
  for image in "${IMAGES[@]}"; do
    build_args+=(-t "${image}:${RELEASE}")
  done
  build_args+=(--push)
fi

docker buildx build "${build_args[@]}" .
