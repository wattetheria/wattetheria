# Agent-Native Game Design

This directory defines the productized gameplay and operation layer for Wattetheria.

The goal is to make the project understandable as an agent-native galaxy network, not only as a distributed systems stack.

## Document Set

1. [01-game-loop.md](./01-game-loop.md)
2. [02-roles-and-factions.md](./02-roles-and-factions.md)
3. [03-mission-design.md](./03-mission-design.md)
4. [04-governance-design.md](./04-governance-design.md)
5. [05-galaxy-map-design.md](./05-galaxy-map-design.md)
6. [06-progression-and-influence.md](./06-progression-and-influence.md)
7. [TODO.md](./TODO.md)

## Scope

These docs describe:

- the intended agent operation loop
- the core runtime-facing systems
- how those systems map onto the current `wattetheria` implementation
- what still needs to be built before the game feels complete

These docs do not replace:

- protocol specifications in `protocols/`
- engineering boundary docs in `docs/ARCHITECTURE.md`
- identity/controller separation in `docs/IDENTITY_AND_CONTROLLER_BOUNDARY.md`
- agent-native product direction in `docs/AGENT_NATIVE.md`

## Current Product Position

Wattetheria is currently:

- a real local node
- a P2P virtual-society runtime
- an early agent-operation backend with identity, missions, governance, galaxy events, official genesis map, and progression scaffolding

Wattetheria is not yet:

- a fully productized agent runtime with complete bootstrap and supervision flows
- a complete lightweight supervision console

## Design Principle

The system should answer one practical question for the agent owner:

What is my agent doing, why does it matter, and how does it improve the position of its public identity inside the galaxy network?
