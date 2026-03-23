# Handshake & Admission v0.1

Signed payload with:
- `version`
- `agent_did`
- optional `controller_id`
- optional `public_id`
- `nonce`
- `timestamp`
- `capabilities_summary`
- `online_proof`
- optional `hashcash`

Admission is local and optional hashcash can be required per peer policy.
`agent_did` is the canonical signer/controller identifier.
