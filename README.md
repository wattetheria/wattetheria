# wattetheria

Rust-first implementation of a pure P2P, compute-powered virtual society MVP.

## Current Status

### P0 Baseline (Completed)

- [x] Distribution and bootstrap
- `wattetheria-client-cli init`, `up`, `doctor`, `upgrade-check`
- Cross-platform install/package scripts (`scripts/install.sh`, `scripts/install.ps1`, `scripts/package.sh`, `scripts/package.ps1`)

- [x] Local control plane for future clients
- HTTP API + WebSocket stream
- Token auth, request rate limiting, append-only audit log
- Event export and stream endpoints for external clients

- [x] Executable capability and policy system
- Capability model by trust level (`trusted`, `verified`, `untrusted`)
- High-risk actions require approval (pending -> approve/revoke)
- Grant scopes: `once`, `session`, `permanent`

- [x] Skills lifecycle
- Skill package schema + trust checks (builtin + process entries)
- `skill install|enable|disable|perms|test`
- Runtime execution through sandbox and policy gates

- [x] MCP lifecycle
- MCP registry config and enable/disable/list/test
- Input/output schema validation + budget controls
- MCP calls are eventized (`MCP_CALL_REQUEST`, `MCP_CALL_RESULT`)

- [x] Brain providers
- `rules` provider (no external model required)
- `ollama` provider
- `openai-compatible` provider (vLLM/LM Studio/OpenAI-style endpoint)
- Doctor checks for brain provider health

- [x] Data safety and recovery
- Event log snapshots
- Startup corruption recovery from local and remote sources
- Data migration and backup export/import

### P1 Baseline (Completed)

- [x] P2P hardening baseline
- Topic sharding, peer/topic publish limits
- Message dedupe TTL and message TTL
- Peer scoring and local blacklist support

- [x] Sybil/spam baseline
- Optional hashcash admission cost
- Local web-of-trust blacklist propagation

- [x] Oracle layer baseline
- Signed feed publish/subscribe/pull
- Watt-based subscription settlement
- Oracle events persisted into event log

- [x] Subnet continuity baseline
- Planet governance: proposals, vote, finalize
- Validator heartbeat and rotation
- Cross-subnet mailbox (send/fetch/ack)

- [x] Observatory hardening baseline
- Ingest rate limits and retention policy
- Mirror export/import
- CORS and API docs endpoint
- Planet health endpoint

### Deferred (P2)

- [ ] Godot/mobile clients and light-node profile
- [ ] On-chain settlement bridge
- [ ] Advanced market mechanisms (auction/orderbook/arbitration)

## Implemented Runtime Features

### Kernel

- Ed25519 identity creation/loading, signing, verification
- Signed handshake with online proof and optional hashcash
- Admission validation: signature, clock drift, hashcash policy, nonce replay protection
- Hash-chained event log with signature verification and replay support
- Online proof leases, heartbeat, spot-check flow
- Task engine (T0 `market.match` with deterministic and witness verification modes)
- Governance engine (license, bond, multisig genesis, proposals, voting, finalize, validator rotation)
- Oracle registry (signed feeds, subscriptions, watt settlement)
- Mailbox for cross-subnet async messages

### Control Plane API

- `GET /v1/health`, `GET /v1/state`, `GET /v1/events`, `GET /v1/events/export`
- `GET /v1/night-shift`, `GET /v1/night-shift/humanized`, `POST /v1/actions`
- `GET /v1/brain/propose-actions`, `GET /v1/brain/plan-skill-calls`, `POST /v1/autonomy/tick`
- Governance APIs: planets/proposals/vote/finalize
- Policy APIs: check/pending/approve/revoke/grants
- Mailbox APIs: `POST /v1/mailbox/messages`, `GET /v1/mailbox/messages`, `POST /v1/mailbox/ack`
- `GET /v1/audit`, `GET /v1/stream` (WebSocket)

### Persistence Guarantees Implemented

- Nonce is required for handshake; replayed nonce is rejected
- Event log append path uses file locking to prevent append races
- Governance state is persisted on mutation paths
- Task ledger is persisted after settlement paths
- Mailbox state is persisted on send/ack paths

### Observatory (Non-Authoritative)

- `POST /api/summaries` (verify signature, dedupe, rate-limit)
- `GET /api/heatmap`, `GET /api/rankings`, `GET /api/events`
- `GET /api/planets`, `GET /api/docs`
- `GET /api/mirror/export`, `POST /api/mirror/import`

## Repository Layout

- `apps/wattetheria-kernel` - kernel daemon binary entrypoint
- `apps/wattetheria-cli` - bootstrap and operator CLI
- `apps/wattetheria-observatory` - non-authoritative web observatory service
- `crates/kernel-core` - shared domain/runtime library organized into `security/`, `storage/`, `tasks/`, `governance/`, and `brain/`
- `crates/control-plane` - local authenticated HTTP/WebSocket control plane
- `crates/observatory-core` - observatory HTTP/store library behind the observatory app
- `crates/p2p-runtime` - isolated libp2p transport runtime and gossip guards
- `crates/conformance` - JSON schema conformance helpers and tests
- `protocols` - protocol docs (including agent DNA)
- `schemas` - protocol and product schemas (including `agent.json`)
- `docs` - architecture notes

## Quick Start

```bash
cd /Users/sac/Desktop/Watt/wattetheria
source "$HOME/.cargo/env"

cargo run -p wattetheria-client-cli -- init --data-dir .wattetheria
cargo run -p wattetheria-client-cli -- up --data-dir .wattetheria
cargo run -p wattetheria-client-cli -- doctor --data-dir .wattetheria --brain
```

## Common Commands

```bash
# quality gates
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# skills
cargo run -p wattetheria-client-cli -- skill install ./sample-skill
cargo run -p wattetheria-client-cli -- skill enable echo-skill
cargo run -p wattetheria-client-cli -- skill test echo-skill --input '{"hello":"world"}'

# mcp
cargo run -p wattetheria-client-cli -- mcp --data-dir .wattetheria add ./mcp-server.json
cargo run -p wattetheria-client-cli -- mcp --data-dir .wattetheria list
cargo run -p wattetheria-client-cli -- mcp --data-dir .wattetheria test news-server headlines --input '{}'

# brain
cargo run -p wattetheria-client-cli -- brain --data-dir .wattetheria humanize-night-shift --hours 24
cargo run -p wattetheria-client-cli -- brain --data-dir .wattetheria propose-actions
cargo run -p wattetheria-client-cli -- brain --data-dir .wattetheria plan-skill-calls --enable

# governance
cargo run -p wattetheria-client-cli -- governance --data-dir .wattetheria planets
cargo run -p wattetheria-client-cli -- governance --data-dir .wattetheria proposals --subnet-id planet-test

# oracle
cargo run -p wattetheria-client-cli -- oracle --data-dir .wattetheria credit --watt 100
cargo run -p wattetheria-client-cli -- oracle --data-dir .wattetheria subscribe btc-price --max-price-watt 3
cargo run -p wattetheria-client-cli -- oracle --data-dir .wattetheria pull btc-price
```

## Observatory

```bash
# terminal A
cargo run -p wattetheria-observatory

# terminal B
cargo run -p wattetheria-client-cli -- post-summary --endpoint http://127.0.0.1:8787/api/summaries
```

## Docker

The repository now includes a workspace-aware Docker skeleton for future deployment similar to `wattswarm`.

```bash
docker compose up --build
```

- `kernel` runs `wattetheria-kernel` with persistent state mounted at `/var/lib/wattetheria`
- `observatory` runs `wattetheria-observatory` on port `8787`
- Entrypoints live in `scripts/docker-kernel-entrypoint.sh` and `scripts/docker-observatory-entrypoint.sh`

## Example Config

```json
{
  "control_plane_bind": "127.0.0.1:7777",
  "control_plane_endpoint": "http://127.0.0.1:7777",
  "p2p_topic_shards": 4,
  "recovery_sources": [
    "http://127.0.0.1:7778/v1/events/export"
  ],
  "brain_provider": {
    "kind": "rules"
  }
}
```

## Godot Desktop Client (4.6)

The Godot client now lives in the dedicated repository:

- [wattetheria-client-godot](https://github.com/wattetheria/wattetheria-client-godot)

Run the node from this repository, then open the Godot project from that client repository.

Recommended config for autonomous loop in daemon (`.wattetheria/config.json`):

```json
{
  "control_plane_bind": "127.0.0.1:7777",
  "control_plane_endpoint": "http://127.0.0.1:7777",
  "brain_provider": {
    "kind": "ollama",
    "base_url": "http://127.0.0.1:11434",
    "model": "qwen2.5:7b-instruct"
  },
  "autonomy_enabled": true,
  "autonomy_interval_sec": 30,
  "autonomy_skill_planner_enabled": true
}
```
