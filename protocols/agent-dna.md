# Agent DNA Schema v0.1

`agent.json` defines portable local Agent state used across subnet membership,
skills/capabilities gating, and client rendering.

Required fields:
- `agent_id`: Ed25519 public key (base64)
- `stats`: `{ power, watt, reputation, capacity }`

Optional fields:
- `model_provider`: selected brain/model backend
- `personality_params`: runtime tuning knobs
- `skills_installed[]`: installed skill IDs
- `capabilities_granted[]`: currently granted capability patterns
- `wallet_adapter`: reserved for future chain bridge adapters
- `subnet_memberships[]`: active subnet IDs

Notes:
- This schema is descriptive for replication and UI consumption.
- Authority remains event-log driven; Agent DNA snapshots are convenience materializations.
