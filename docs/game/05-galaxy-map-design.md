# 05. Galaxy Map Design

## Purpose

The galaxy map is not background art. It is a gameplay object.

It should host:

- missions
- governance
- route pressure
- expansion
- future community-authored content

## Official Foundation

The first layer is the official genesis map.

It exists to provide:

- a guaranteed starting network
- a stable bootstrap space
- canonical starter systems and routes
- a fixed anchor for later community expansion

## Current Official Base

The current official map is `genesis-base`.

It contains:

- 3 systems
- starter planets
- 2 canonical routes
- zone alignment to `genesis-core`, `frontier-belt`, and `deep-space`

## Map Object Model

The current object model is correct for a first release:

- `GalaxyMap`
- `StarSystem`
- `PlanetNode`
- `RouteEdge`

## Why Map Must Stay Separate From Civilization

The map is not just one civilization feature.

It is a separate domain because it will eventually own:

- official map state
- validation
- persisted travel state
- route interaction

That is why it belongs in `crates/kernel-core/src/map`, not under `civilization`.

## Current Backend Coverage

Already implemented in code:

- official genesis map model
- registry
- validation
- persistence
- read-only map endpoints
- route-travel planning endpoints for direct options and recommended paths
- persisted travel state with current location and active travel session
- depart and arrive endpoints for map-driven movement
- mission client views that classify reachable local work versus missions that require travel to another system
- arrival consequences that summarize newly reachable local missions, route risk, and any governed subnet anchored to the destination

Still missing:

- richer travel consequences on mission state mutation, governance pressure shifts, and longer public-memory trails
- lightweight map authoring and supervision workflow
