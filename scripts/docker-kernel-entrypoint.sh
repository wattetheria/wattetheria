#!/bin/sh
set -eu

DATA_DIR="${WATTETHERIA_DATA_DIR:-/var/lib/wattetheria}"
CONTROL_PLANE_BIND="${WATTETHERIA_CONTROL_PLANE_BIND:-0.0.0.0:7777}"
AUTONOMY_INTERVAL_SEC="${WATTETHERIA_AUTONOMY_INTERVAL_SEC:-30}"

set -- \
  /app/target/release/wattetheria-kernel \
  --data-dir "${DATA_DIR}" \
  --control-plane-bind "${CONTROL_PLANE_BIND}" \
  --autonomy-interval-sec "${AUTONOMY_INTERVAL_SEC}"

if [ "${WATTETHERIA_AUTONOMY_ENABLED:-false}" = "true" ]; then
  set -- "$@" --autonomy-enabled
fi

if [ -n "${WATTETHERIA_BRAIN_PROVIDER_KIND:-}" ]; then
  set -- "$@" --brain-provider-kind "${WATTETHERIA_BRAIN_PROVIDER_KIND}"
fi

if [ -n "${WATTETHERIA_BRAIN_BASE_URL:-}" ]; then
  set -- "$@" --brain-base-url "${WATTETHERIA_BRAIN_BASE_URL}"
fi

if [ -n "${WATTETHERIA_BRAIN_MODEL:-}" ]; then
  set -- "$@" --brain-model "${WATTETHERIA_BRAIN_MODEL}"
fi

if [ -n "${WATTETHERIA_BRAIN_API_KEY_ENV:-}" ]; then
  set -- "$@" --brain-api-key-env "${WATTETHERIA_BRAIN_API_KEY_ENV}"
fi

if [ -n "${WATTETHERIA_WATTSWARM_UI_BASE_URL:-}" ]; then
  set -- "$@" --wattswarm-ui-base-url "${WATTETHERIA_WATTSWARM_UI_BASE_URL}"
fi

if [ -n "${WATTETHERIA_WATTSWARM_SYNC_GRPC_ENDPOINT:-}" ]; then
  set -- "$@" --wattswarm-sync-grpc-endpoint "${WATTETHERIA_WATTSWARM_SYNC_GRPC_ENDPOINT}"
fi

if [ -n "${WATTETHERIA_AGENT_CONTROL_PLANE_ENDPOINT:-}" ]; then
  set -- "$@" --agent-control-plane-endpoint "${WATTETHERIA_AGENT_CONTROL_PLANE_ENDPOINT}"
fi

if [ -n "${WATTETHERIA_AGENT_WATTSWARM_UI_BASE_URL:-}" ]; then
  set -- "$@" --agent-wattswarm-ui-base-url "${WATTETHERIA_AGENT_WATTSWARM_UI_BASE_URL}"
fi

if [ -n "${WATTETHERIA_AGENT_WATTSWARM_SYNC_GRPC_ENDPOINT:-}" ]; then
  set -- "$@" --agent-wattswarm-sync-grpc-endpoint "${WATTETHERIA_AGENT_WATTSWARM_SYNC_GRPC_ENDPOINT}"
fi

if [ -n "${WATTETHERIA_AGENT_HOST_DATA_DIR:-}" ]; then
  set -- "$@" --agent-host-data-dir "${WATTETHERIA_AGENT_HOST_DATA_DIR}"
fi

if [ -n "${WATTETHERIA_SERVICENET_BASE_URL:-}" ]; then
  set -- "$@" --servicenet-base-url "${WATTETHERIA_SERVICENET_BASE_URL}"
fi

exec "$@"
