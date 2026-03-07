# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Wattetheria is a Rust-first P2P compute-powered virtual society MVP. It implements event-sourced identity, task markets, governance, oracle feeds, and capability-based security across a decentralized network.

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

# Skills
cargo run -p wattetheria-client-cli -- skill install ./sample-skill
cargo run -p wattetheria-client-cli -- skill enable echo-skill
cargo run -p wattetheria-client-cli -- skill test echo-skill --input '{"hello":"world"}'

# MCP
cargo run -p wattetheria-client-cli -- mcp --data-dir .wattetheria add ./mcp-server.json
cargo run -p wattetheria-client-cli -- mcp --data-dir .wattetheria list
cargo run -p wattetheria-client-cli -- mcp --data-dir .wattetheria test news-server headlines --input '{}'

# Brain
cargo run -p wattetheria-client-cli -- brain --data-dir .wattetheria humanize-night-shift --hours 24
cargo run -p wattetheria-client-cli -- brain --data-dir .wattetheria propose-actions
cargo run -p wattetheria-client-cli -- brain --data-dir .wattetheria plan-skill-calls --enable

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
- **`apps/wattetheria-cli`** (`wattetheria-client-cli`) — CLI entry point with subcommands (`init`, `up`, `doctor`, `upgrade-check`, `policy`, `governance`, `skill`, `mcp`, `brain`, `data`, `oracle`, `night-shift`, `post-summary`).
- **`apps/wattetheria-observatory`** (`wattetheria-observatory`) — Non-authoritative HTTP explorer service.
- **`crates/kernel-core`** (`wattetheria-kernel-core`) — Core daemon and domain engine library, internally grouped into `security/`, `storage/`, `tasks/`, `governance/`, and `brain/`.
- **`crates/control-plane`** (`wattetheria-control-plane`) — Authenticated Axum control plane and autonomy routes.
- **`crates/observatory-core`** (`wattetheria-observatory-core`) — Observatory store/router library used by the observatory app.
- **`crates/p2p-runtime`** (`wattetheria-p2p-runtime`) — Isolated libp2p runtime and anti-spam transport guards.
- **`crates/conformance`** (`wattetheria-conformance`) — JSON Schema validation library. Loads schemas from `schemas/` directory at runtime.

Supporting directories: `protocols/` (protocol specs including agent DNA), `schemas/` (JSON Schema draft 2020-12, including `agent.json`), `docs/`, `scripts/` (cross-platform install/package scripts).

## Architecture

### Layered Design

1. **Identity & Crypto** — Ed25519 keys (`crates/kernel-core/src/security/identity.rs`), canonical JSON signing via `serde_jcs` (`crates/kernel-core/src/security/signing.rs`).
2. **Event Sourcing** — Append-only JSONL log with SHA256 hash chains (`crates/kernel-core/src/storage/event_log.rs`). All state changes are events.
3. **P2P Network** — libp2p with gossipsub + kademlia + identify + noise (`crates/p2p-runtime/src/lib.rs`). Anti-spam via per-peer/per-topic rate limits, message dedup TTL, peer scoring, blacklist.
4. **Control Plane** — Axum HTTP + WebSocket API with token auth, rate limiting, audit log (`crates/control-plane/src/`).
5. **Task Engine** — Deterministic task lifecycle: PUBLISHED → CLAIMED → EXECUTED → SUBMITTED → VERIFIED → SETTLED (`crates/kernel-core/src/tasks/task_engine.rs`). Market matching for buy/sell orders. Settles `watt`, `reputation`, `capacity`.
6. **Capabilities** — Trust levels (Trusted/Verified/Untrusted) with default-deny policy engine (`crates/kernel-core/src/security/capabilities.rs`, `crates/kernel-core/src/brain/policy_engine.rs`). Grants scoped as Once/Session/Permanent.
7. **Extensions** — Skill packages (`crates/kernel-core/src/brain/skill_package.rs`), MCP adapter (`crates/kernel-core/src/brain/mcp.rs`), brain providers (`crates/kernel-core/src/brain/brain.rs`: rules/ollama/openai-compatible).
8. **Governance** — Planet (subnet) creation, proposals, voting, validator rotation (`crates/kernel-core/src/governance/governance.rs`). Cross-subnet mailbox (`crates/kernel-core/src/governance/mailbox.rs`).
9. **Oracle** — Signed feeds, subscriptions, watt-based settlement (`crates/kernel-core/src/governance/oracle.rs`).

### Key Patterns

- **Load-or-Create**: Modules use `load_or_create()` / `load_or_new()` for persistent state with explicit `persist()` / `save()` methods. State is JSON file-based.
- **Error Handling**: `anyhow::Result<T>` with `.context()` for propagation; `thiserror::Error` for domain errors; `bail!()` for immediate failure.
- **Signing**: Struct → canonical JSON (`serde_jcs`) → Ed25519 sign → embed signature.
- **Async**: Full tokio async throughout. `async_trait` for trait impls. `tokio::sync::Mutex` for shared state.

## Control Plane API Endpoints

- `GET /v1/health`, `GET /v1/state`, `GET /v1/events`, `GET /v1/events/export`
- `GET /v1/night-shift`, `POST /v1/actions`
- Governance: planets/proposals/vote/finalize
- Policy: check/pending/approve/revoke/grants
- Mailbox: `POST /v1/mailbox/messages`, `GET /v1/mailbox/messages`, `POST /v1/mailbox/ack`
- `GET /v1/audit`, `GET /v1/stream` (WebSocket)

## Observatory API Endpoints

- `POST /api/summaries` (verify signature, dedupe, rate-limit)
- `GET /api/heatmap`, `GET /api/rankings`, `GET /api/events`
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
- Key integration tests: `pipeline_integration.rs` (end-to-end task→summary→governance→mailbox), `skill_runtime_integration.rs`, `eventlog_integration.rs`, `product_iteration_integration.rs`
- Tests use `tempfile::tempdir()` for isolated filesystem state

## Data Directory Layout

Node state lives in a configurable data dir (default `.wattetheria`):
- `identity.json` — Ed25519 keypair
- `events.jsonl` — Hash-chained event log
- `control.token` — Bearer token for HTTP API
- `policy/state.json` — Capability grants and pending requests
- `audit/control_plane.jsonl` — Audit trail
- `skills/` — Installed skill packages
- `mcp/config.json` — MCP server configs
- `oracle/registry.json` — Oracle feed registry

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
