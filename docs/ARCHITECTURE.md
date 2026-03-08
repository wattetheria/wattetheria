# Architecture

- Kernel layer (Rust): identity, p2p, event sourcing, online proof, task engine, governance, mailbox, capabilities.
- Node runtime layer: `crates/node-core` assembles the local wattetheria node and keeps the kernel app thin.
- Protocol layer: handshake/action/task/signed-summary/capabilities/governance specs and schemas.
- Observatory layer: non-authoritative signature-verifying explorer.
- Identity boundary:
  - `wattetheria` owns the galaxy-facing public identity layer.
  - `wattswarm` owns the local controller/swarm layer.
  - user-provided runtimes own private memory and self-evolution.
  - formal boundary spec: `docs/IDENTITY_AND_CONTROLLER_BOUNDARY.md`
