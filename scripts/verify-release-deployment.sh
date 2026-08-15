#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
COMPOSE_FILE="$ROOT_DIR/docker-compose.release.yml"
FULL_COMPOSE_FILE="$ROOT_DIR/docker-compose.full.yml"
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
require_file "$FULL_COMPOSE_FILE"
require_file "$ENV_FILE"

require_text "$COMPOSE_FILE" "WATTETHERIA_GATEWAY_URLS" "kernel must accept explicit gateway URLs"
require_text "$COMPOSE_FILE" "WATTETHERIA_GATEWAY_CONFIG_PATH" "kernel must read the Wattswarm startup config"
require_text "$COMPOSE_FILE" "WATTSWARM_IROH_DATA_PLANE_START_TIMEOUT_MS" "Wattswarm kernel must receive the Iroh data plane startup timeout"
require_text "$COMPOSE_FILE" "WATTSWARM_IROH_PUBLISH_DIRECT_ADDRS" "Wattswarm kernel must receive the Iroh direct publishing policy"
require_text "$COMPOSE_FILE" 'WATTSWARM_NETWORK_BACKEND: ${WATTSWARM_NETWORK_BACKEND:-p2p}' "Wattswarm kernel must receive the selected network backend"
require_text "$COMPOSE_FILE" 'WATTSWARM_CLIENT_SERVER_URL: ${WATTSWARM_CLIENT_SERVER_URL:-}' "Wattswarm kernel must receive the ClientServer Message Gateway URL"
require_text "$COMPOSE_FILE" 'WATTSWARM_NETWORK_REGISTRATION_URL: ${WATTSWARM_NETWORK_REGISTRATION_URL:-}' "Wattswarm kernel must receive the optional Genesis registration URL"
require_text "$COMPOSE_FILE" 'WATTSWARM_CS_AUTO_REGISTER: ${WATTSWARM_CS_AUTO_REGISTER:-true}' "Wattswarm kernel must receive the ClientServer auto-registration switch"
require_text "$COMPOSE_FILE" 'WATTSWARM_NETWORK_GRANT_TTL_SECONDS: ${WATTSWARM_NETWORK_GRANT_TTL_SECONDS:-0}' "Wattswarm Genesis must receive the Grant TTL"
require_text "$FULL_COMPOSE_FILE" 'WATTSWARM_NETWORK_BACKEND: ${WATTSWARM_NETWORK_BACKEND:-p2p}' "development Wattswarm kernel must receive the selected network backend"
require_text "$FULL_COMPOSE_FILE" 'WATTSWARM_CLIENT_SERVER_URL: ${WATTSWARM_CLIENT_SERVER_URL:-}' "development Wattswarm kernel must receive the ClientServer Message Gateway URL"
require_text "$FULL_COMPOSE_FILE" 'WATTSWARM_NETWORK_REGISTRATION_URL: ${WATTSWARM_NETWORK_REGISTRATION_URL:-}' "development Wattswarm kernel must receive the optional Genesis registration URL"
require_text "$FULL_COMPOSE_FILE" 'WATTSWARM_CS_AUTO_REGISTER: ${WATTSWARM_CS_AUTO_REGISTER:-true}' "development Wattswarm kernel must receive the ClientServer auto-registration switch"
require_text "$FULL_COMPOSE_FILE" 'WATTSWARM_NETWORK_GRANT_TTL_SECONDS: ${WATTSWARM_NETWORK_GRANT_TTL_SECONDS:-0}' "development Wattswarm Genesis must receive the Grant TTL"
require_text "$COMPOSE_FILE" 'WATTSWARM_NETWORK_SERVICE_ENABLED: "false"' "release worker must not start a second network service"
require_text "$FULL_COMPOSE_FILE" 'WATTSWARM_NETWORK_SERVICE_ENABLED: "false"' "development worker must not start a second network service"
require_text "$COMPOSE_FILE" 'WATTSWARM_STORAGE_BACKEND: ${WATTSWARM_STORAGE_BACKEND:-postgres}' "Wattswarm services must receive the selected storage backend"
require_text "$COMPOSE_FILE" "WATTSWARM_STORE_NAME: wattswarm.db" "release Wattswarm services must use the unified SQLite filename"
require_text "$FULL_COMPOSE_FILE" "WATTSWARM_STORE_NAME: wattswarm.db" "development Wattswarm services must use the unified SQLite filename"
require_text "$COMPOSE_FILE" "/var/lib/wattswarm/startup_config.json" "gateway config path must point at the mounted Wattswarm state"
require_text "$COMPOSE_FILE" '${WATTSWARM_HOST_STATE_DIR:-./data/wattswarm}:/var/lib/wattswarm:ro' "kernel must mount Wattswarm state read-only"
require_text "$ENV_FILE" "WATTSWARM_HOST_STATE_DIR=./data/wattswarm" "release env template must define the Wattswarm host state directory"
require_text "$ENV_FILE" "WATTSWARM_STORAGE_BACKEND=postgres" "release env template must default Wattswarm storage to PostgreSQL"
require_text "$ENV_FILE" "WATTSWARM_NETWORK_BACKEND=p2p" "release env template must default Wattswarm networking to P2P"
require_text "$ENV_FILE" "WATTSWARM_CLIENT_SERVER_URL=" "release env template must include the ClientServer Message Gateway URL slot"
require_text "$ENV_FILE" "WATTSWARM_NETWORK_REGISTRATION_URL=" "release env template must include the optional Genesis registration URL slot"
require_text "$ENV_FILE" "WATTSWARM_CS_AUTO_REGISTER=true" "release env template must enable ClientServer auto registration by default"
require_text "$ENV_FILE" "WATTSWARM_NETWORK_REGISTRATION_URL=https://bootstrap.wattetheria.com" "release env template must publish the Genesis registration base URL"
require_text "$ENV_FILE" "WATTSWARM_NETWORK_GRANT_TTL_SECONDS=0" "release env template must keep Grant expiry disabled by default"
require_text "$ENV_FILE" "WATTETHERIA_WATTSWARM_AGENT_EVENT_CALLBACK_BASE_URL=http://kernel:7777" "release env template must define the internal agent event callback base URL"
require_text "$ENV_FILE" "WATTETHERIA_BRAIN_API_KEY=" "release env template must include the concrete brain API key value slot"
require_text "$ENV_FILE" "WATTETHERIA_BRAIN_SESSION_MODE=stable_per_scope" "release env template must default to scoped stable runtime sessions"
require_text "$ENV_FILE" "WATTSWARM_IROH_HOST_PORT=4002" "release env template must define the host Iroh UDP port"
require_text "$ENV_FILE" "WATTSWARM_IROH_BIND_ADDR=0.0.0.0:4002" "release env template must define the fixed Iroh UDP bind address"
require_text "$ENV_FILE" "WATTSWARM_IROH_PUBLISH_DIRECT_ADDRS=false" "release env template must publish Iroh direct addresses by default"
require_text "$ENV_FILE" "WATTSWARM_IROH_DATA_PLANE_START_TIMEOUT_MS=120000" "release env template must define the Iroh data plane startup timeout"
require_text "$ENV_FILE" "WATTETHERIA_GATEWAY_CONFIG_PATH=/var/lib/wattswarm/startup_config.json" "release env template must expose the gateway config path"

storage_backend_count="$(grep -Fc 'WATTSWARM_STORAGE_BACKEND: ${WATTSWARM_STORAGE_BACKEND:-postgres}' "$COMPOSE_FILE")"
if [ "$storage_backend_count" -ne 2 ]; then
  echo "release deployment check failed: Wattswarm kernel and worker must both receive the storage backend" >&2
  exit 1
fi

release_store_count="$(grep -Fc 'WATTSWARM_STORE_NAME: wattswarm.db' "$COMPOSE_FILE")"
full_store_count="$(grep -Fc 'WATTSWARM_STORE_NAME: wattswarm.db' "$FULL_COMPOSE_FILE")"
if [ "$release_store_count" -ne 2 ] || [ "$full_store_count" -ne 2 ]; then
  echo "release deployment check failed: Wattswarm kernel and worker must share wattswarm.db" >&2
  exit 1
fi

if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
  docker compose --env-file "$ENV_FILE" -f "$COMPOSE_FILE" config >/dev/null
fi

echo "release deployment artifacts verified"
