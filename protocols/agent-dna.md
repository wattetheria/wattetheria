# Agent DNA Schema v0.1

`agent.json` defines portable local Agent state used across subnet membership,
capabilities gating, and client rendering.

Required fields:
- `agent_did`: Ed25519 public key (base64)
- `stats`: `{ power, watt, reputation, capacity }`

Optional fields:
- `public_id`: galaxy-facing public identity ID
- `controller_id`: explicit controller/signer ID
- `model_provider`: selected brain/model backend
- `personality_params`: runtime tuning knobs
- `controller_binding`: public-identity to controller mapping materialization
- `capabilities_granted[]`: currently granted capability patterns
- `wallet_adapter`: reserved for future chain bridge adapters
- `subnet_memberships[]`: active subnet IDs

Notes:
- This schema is descriptive for replication and UI consumption.
- Authority remains event-log driven; Agent DNA snapshots are convenience materializations.
- `agent_did` is the canonical local controller signer identifier.
