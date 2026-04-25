#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
COMPOSE_FILE="$ROOT_DIR/docker-compose.release.yml"
ENV_FILE="$ROOT_DIR/.env.release"

require_file() {
  local path="$1"
  if [ ! -f "$path" ]; then
    echo "missing required release file: $path" >&2
    exit 1
  fi
}

require_text() {
  local path="$1"
  local pattern="$2"
  local description="$3"
  if ! grep -Fq "$pattern" "$path"; then
    echo "release deployment check failed: $description" >&2
    echo "expected to find: $pattern" >&2
    exit 1
  fi
}

require_file "$COMPOSE_FILE"
require_file "$ENV_FILE"

require_text "$COMPOSE_FILE" "WATTETHERIA_GATEWAY_URLS" "kernel must accept explicit gateway URLs"
require_text "$COMPOSE_FILE" "WATTETHERIA_GATEWAY_CONFIG_PATH" "kernel must read the Wattswarm startup config"
require_text "$COMPOSE_FILE" "/var/lib/wattswarm/startup_config.json" "gateway config path must point at the mounted Wattswarm state"
require_text "$COMPOSE_FILE" '${WATTSWARM_HOST_STATE_DIR:-./data/wattswarm}:/var/lib/wattswarm:ro' "kernel must mount Wattswarm state read-only"
require_text "$ENV_FILE" "WATTSWARM_HOST_STATE_DIR=./data/wattswarm" "release env template must define the Wattswarm host state directory"

if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
  docker compose --env-file "$ENV_FILE" -f "$COMPOSE_FILE" config >/dev/null
fi

echo "release deployment artifacts verified"
