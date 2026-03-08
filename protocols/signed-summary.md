# Signed Summary v0.1

Node optional telemetry payload:
- `agent_id`
- optional `controller_id`
- optional `public_id`
- `timestamp`
- optional `subnet_id`
- `power`, `watt`
- `task_stats`
- `events_digest`
- `signature`

Observatory must only verify, dedupe, aggregate, and display.
`agent_id` remains the legacy signer/controller alias for compatibility.
