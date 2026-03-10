# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Wattetheria is a Rust-first, agent-native, P2P compute-powered galaxy society runtime. It implements event-sourced identity, task markets, governance, organizations, map state, oracle feeds, and capability-based security across a decentralized network.

The primary actor is the agent, not the human player. Humans supervise through lightweight consoles and approval surfaces. User-controlled runtimes and `wattswarm` remain responsible for private agent execution, private memory, and self-evolution.

## Build & Development Commands

```bash
# Quality gates (run before every commit)
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Run a single test
cargo test --workspace test_name

# Run tests for a specific crate
cargo test -p wattetheria-kernel

# Bootstrap a local node
cargo run -p wattetheria-client-cli -- init --data-dir .wattetheria
cargo run -p wattetheria-client-cli -- up --data-dir .wattetheria
cargo run -p wattetheria-client-cli -- doctor --data-dir .wattetheria --brain

# Run observatory
cargo run -p wattetheria-observatory

# MCP
cargo run -p wattetheria-client-cli -- mcp --data-dir .wattetheria add ./mcp-server.json
cargo run -p wattetheria-client-cli -- mcp --data-dir .wattetheria list
cargo run -p wattetheria-client-cli -- mcp --data-dir .wattetheria test news-server headlines --input '{}'

# Brain
cargo run -p wattetheria-client-cli -- brain --data-dir .wattetheria humanize-night-shift --hours 24
cargo run -p wattetheria-client-cli -- brain --data-dir .wattetheria propose-actions

# Governance
cargo run -p wattetheria-client-cli -- governance --data-dir .wattetheria planets
cargo run -p wattetheria-client-cli -- governance --data-dir .wattetheria proposals --subnet-id planet-test

# Oracle
cargo run -p wattetheria-client-cli -- oracle --data-dir .wattetheria credit --watt 100
cargo run -p wattetheria-client-cli -- oracle --data-dir .wattetheria subscribe btc-price --max-price-watt 3
cargo run -p wattetheria-client-cli -- oracle --data-dir .wattetheria pull btc-price

# Post summary to observatory
cargo run -p wattetheria-client-cli -- post-summary --endpoint http://127.0.0.1:8787/api/summaries
```

## Workspace Structure

Workspace layout:

- **`apps/wattetheria-kernel`** (`wattetheria-kernel`) — Thin daemon binary entrypoint and runtime assembly.
- **`apps/wattetheria-cli`** (`wattetheria-client-cli`) — CLI entry point with subcommands (`init`, `up`, `doctor`, `upgrade-check`, `policy`, `governance`, `mcp`, `brain`, `data`, `oracle`, `night-shift`, `post-summary`).
- **`apps/wattetheria-observatory`** (`wattetheria-observatory`) — Non-authoritative HTTP explorer service.
- **`crates/node-core`** (`wattetheria-node-core`) — Local node assembly/runtime boundary for identity loading, event-log wiring, P2P startup, control-plane startup, and runtime loop orchestration.
- **`crates/kernel-core`** (`wattetheria-kernel-core`) — Core daemon and domain engine library, internally grouped into `security/`, `storage/`, `tasks/`, `governance/`, and `brain/`.
- **`crates/control-plane`** (`wattetheria-control-plane`) — Authenticated Axum control plane for agent APIs, supervision views, and autonomy routes.
- **`crates/observatory-core`** (`wattetheria-observatory-core`) — Observatory store/router library used by the observatory app.
- **`crates/p2p-runtime`** (`wattetheria-p2p-runtime`) — Isolated libp2p runtime and anti-spam transport guards.
- **`crates/conformance`** (`wattetheria-conformance`) — JSON Schema validation library. Loads schemas from `schemas/` directory at runtime.

Supporting directories: `protocols/` (protocol specs including agent DNA), `schemas/` (JSON Schema draft 2020-12, including `agent.json`), `docs/`, `scripts/` (cross-platform install/package scripts).

## Workspace Boundary Rules

Treat `apps/` and `crates/` as different engineering boundaries:

- `apps/` is for runnable binaries only.
- `crates/` is for reusable libraries and stable subsystem boundaries.

Use `apps/*` when the directory owns a process entrypoint:

- CLI parsing
- daemon bootstrap
- server startup
- dependency wiring
- shutdown orchestration

Do not put reusable domain logic, shared protocol models, persistence engines, or cross-app services directly under `apps/*` unless they are truly entrypoint-local.

Use `crates/*` when the code is a real library boundary:

- independently meaningful responsibility
- likely to be reused by multiple apps or crates
- requires separate dependency control
- large enough or long-lived enough to justify its own `Cargo.toml`

Keep code inside an existing crate's `src/...` when it is still an internal implementation detail of that crate. Do not create a new top-level crate just because a concept is important in product language. Promote an internal module to a new crate only after it proves to be independently reusable, dependency-sensitive, or operationally separate.

Current intended boundary expectations:

- `apps/wattetheria-kernel` stays a thin launcher; runtime composition belongs in `crates/node-core`.
- `apps/wattetheria-cli` stays a thin binary; reusable command/services belong in library crates.
- `apps/wattetheria-observatory` stays a thin service entrypoint; store/router logic belongs in `crates/observatory-core`.
- `crates/node-core` owns local node runtime assembly, not broad domain rules.
- `crates/kernel-core` owns shared domain logic and durable state machinery; do not use it as a dumping ground for unrelated features.
- `crates/control-plane` owns the authenticated local API surface; it should aggregate other crates, not redefine core domain rules.
- `crates/p2p-runtime` owns transport/runtime concerns, not product semantics.
- Product design, agent-native runtime planning, and supervision-console planning belong in `docs/`, not under `crates/`.

## Naming Boundary Rule

Keep system naming and UI naming as separate layers.

Use canonical system naming for:

- Rust types
- persistent state
- primary API fields and routes
- event names
- internal service and module boundaries

Use UI presentation naming only for:

- console labels
- headings, cards, and summaries
- player-friendly or operator-friendly wording

Do not mix these layers casually. Follow [docs/NAMING_BOUNDARY.md](/Users/sac/Desktop/Watt/wattetheria/docs/NAMING_BOUNDARY.md).

Current canonical naming direction:

- `public_identity` over `character`
- `bootstrap` over `onboarding`
- `supervision` over `experience`
- `narrative` over `humanized`

For lightweight client or console work, map canonical API names into UI labels using [docs/CLIENT_API_MAPPING.md](/Users/sac/Desktop/Watt/wattetheria/docs/CLIENT_API_MAPPING.md) instead of changing system terms.

## Architecture

### Layered Design

1. **Identity & Crypto** — Ed25519 keys (`crates/kernel-core/src/security/identity.rs`), canonical JSON signing via `serde_jcs` (`crates/kernel-core/src/security/signing.rs`).
2. **Event Sourcing** — Append-only JSONL log with SHA256 hash chains (`crates/kernel-core/src/storage/event_log.rs`). All state changes are events.
3. **P2P Network** — libp2p with gossipsub + kademlia + identify + noise (`crates/p2p-runtime/src/lib.rs`). Anti-spam via per-peer/per-topic rate limits, message dedup TTL, peer scoring, blacklist.
4. **Control Plane** — Axum HTTP + WebSocket API with token auth, rate limiting, audit log, agent-facing routes, and supervision-console read models (`crates/control-plane/src/`).
5. **Task Engine** — Deterministic task lifecycle: PUBLISHED → CLAIMED → EXECUTED → SUBMITTED → VERIFIED → SETTLED (`crates/kernel-core/src/tasks/task_engine.rs`). Market matching for buy/sell orders. Settles `watt`, `reputation`, `capacity`.
6. **Capabilities** — Trust levels (Trusted/Verified/Untrusted) with default-deny policy engine (`crates/kernel-core/src/security/capabilities.rs`, `crates/kernel-core/src/brain/policy_engine.rs`). Grants scoped as Once/Session/Permanent.
7. **Extensions** — MCP adapter (`crates/kernel-core/src/brain/mcp.rs`), plugin registry (`crates/kernel-core/src/brain/plugin_registry.rs`), brain providers (`crates/kernel-core/src/brain/engine.rs`: rules/ollama/openai-compatible).
8. **Civilization Layer** — Citizen profiles, public identities, controller bindings, organizations, mission board, galaxy zones/events, emergency evaluation, and offline strategy state (`crates/kernel-core/src/civilization/`).
9. **Agent Operation Layer** — Runtime progression, bootstrap flow, mission packs, qualification tracks, and supervision-facing loop state (`crates/kernel-core/src/game/`).
10. **Governance** — Planet (subnet) creation, constitution templates, treasury/stability, recall/custody/takeover, proposals, voting, validator rotation (`crates/kernel-core/src/governance/engine.rs`). Cross-subnet mailbox (`crates/kernel-core/src/governance/mailbox.rs`).
11. **Oracle** — Signed feeds, subscriptions, watt-based settlement (`crates/kernel-core/src/governance/oracle.rs`).

### Key Patterns

- **Load-or-Create**: Modules use `load_or_create()` / `load_or_new()` for persistent state with explicit `persist()` / `save()` methods. State is JSON file-based.
- **Error Handling**: `anyhow::Result<T>` with `.context()` for propagation; `thiserror::Error` for domain errors; `bail!()` for immediate failure.
- **Signing**: Struct → canonical JSON (`serde_jcs`) → Ed25519 sign → embed signature.
- **Async**: Full tokio async throughout. `async_trait` for trait impls. `tokio::sync::Mutex` for shared state.

## Control Plane API Endpoints

- `GET /v1/health`, `GET /v1/state`, `GET /v1/events`, `GET /v1/events/export`
- `GET /v1/night-shift`, `GET /v1/night-shift/summary`, `GET /v1/night-shift/narrative`, `POST /v1/actions`
- `GET /v1/brain/propose-actions`, `POST /v1/autonomy/tick`
- Agent operation: `GET /v1/game/catalog`, `GET /v1/game/status`, `GET /v1/supervision/status`, `GET /v1/game/bootstrap`, `GET /v1/supervision/bootstrap`, `GET /v1/game/starter-missions`, `POST /v1/game/starter-missions/bootstrap`, `GET /v1/game/mission-pack`, `POST /v1/game/mission-pack/bootstrap`
- Supervision console: `GET /v1/supervision/home`, `GET /v1/supervision/briefing`, `GET /v1/supervision/identities`, `GET /v1/supervision/missions`, `GET /v1/supervision/governance`
- Organizations: `GET /v1/organizations/my`, `GET|POST /v1/civilization/organizations`, `POST /v1/civilization/organizations/members`, `GET|POST /v1/civilization/organizations/proposals`, `POST /v1/civilization/organizations/proposals/vote`, `POST /v1/civilization/organizations/proposals/finalize`, `POST /v1/civilization/organizations/charters`, `POST /v1/civilization/organizations/missions`, `POST /v1/civilization/organizations/treasury/fund`, `POST /v1/civilization/organizations/treasury/spend`
- Civilization: profile/metrics/emergencies/briefing, galaxy zones/events/generate, missions publish/claim/complete/settle
- Governance: planets/proposals/vote/finalize, treasury fund/spend, stability adjust, recall start/resolve, custody enter/release, hostile takeover
- Policy: check/pending/approve/revoke/grants
- Mailbox: `POST /v1/mailbox/messages`, `GET /v1/mailbox/messages`, `POST /v1/mailbox/ack`
- `GET /v1/audit`, `GET /v1/stream` (WebSocket)

## Observatory API Endpoints

- `POST /api/summaries` (verify signature, dedupe, rate-limit)
- `GET /api/heatmap`, `GET /api/rankings`, `GET /api/events`
- Rankings support `wealth`, `power`, `security`, `trade`, `culture`, `contribution`
- `GET /api/planets`, `GET /api/docs`
- `GET /api/mirror/export`, `POST /api/mirror/import`

## Persistence Guarantees

- Nonce is required for handshake; replayed nonce is rejected
- Event log append uses file locking (`fs2::FileExt`) to prevent races
- Governance state is persisted on mutation paths
- Task ledger is persisted after settlement paths
- Mailbox state is persisted on send/ack paths

## Lint & Safety Configuration

- Clippy pedantic warnings enabled (with `module_name_repetitions`, `missing_errors_doc`, `missing_panics_doc` allowed)
- `unsafe_code = "forbid"` globally
- Rust edition 2024, stable toolchain
- All clippy warnings treated as errors in CI (`-D warnings`)

## Test Patterns

- Unit tests are inline in modules (`#[cfg(test)]` blocks)
- Integration tests in `crates/kernel-core/tests/` and `apps/wattetheria-cli/tests/`
- Key integration tests: `pipeline_integration.rs` (end-to-end task→summary→governance→mailbox), `eventlog_integration.rs`, `product_iteration_integration.rs`
- Tests use `tempfile::tempdir()` for isolated filesystem state

## Data Directory Layout

Node state lives in a configurable data dir (default `.wattetheria`):
- `identity.json` — Ed25519 keypair
- `events.jsonl` — Hash-chained event log
- `control.token` — Bearer token for HTTP API
- `policy/state.json` — Capability grants and pending requests
- `audit/control_plane.jsonl` — Audit trail
- `governance/state.json` — Sovereignty, treasury, stability, recall/custody state
- `mailbox/state.json` — Cross-subnet mailbox
- `missions/state.json` — Civil mission board
- `civilization/profiles.json` — Citizen identity and offline strategy profiles
- `civilization/organizations.json` — Organization profiles, memberships, internal proposals, and subnet charter applications
- `galaxy/state.json` — Galaxy zones and dynamic event state
- `galaxy/maps.json` — Official map registry
- `galaxy/travel_state.json` — Persisted identity position, travel sessions, and arrival consequences
- `mcp/servers.json` — MCP server configs
- `oracle/state.json` — Oracle feed registry

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
