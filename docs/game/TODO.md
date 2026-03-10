# Game Design TODO

This file tracks what is still missing between the current `wattetheria` backend and a fully productized gameplay layer.

Status labels:

- `done` — already implemented in code
- `partial` — basic backend exists but productized design or client-facing behavior is incomplete
- `todo` — not implemented yet

## P0: Required For A Cohesive First Playable Product

- `done` Official genesis base map
- `done` Public identity bootstrap with controller binding
- `done` Roles, factions, strategies, and profile persistence
- `done` Mission lifecycle with role/faction qualification
- `done` Governance core: license, bond, proposals, treasury, stability, recall, custody, takeover
- `done` Civilization scoring and total influence
- `done` Game catalog and game status endpoints
- `partial` New-identity bootstrap flow and first-cycle sequencing, with backend bootstrap flow and first-cycle action cards now available
- `done` Role-specific starter objective chains
- `partial` Supervision-home views for a polished first-session experience, including embedded game progression, mission-pack state, map-aware mission/travel summaries, and backend `next_actions / alerts / priority_cards`
- `partial` Explicit organization gameplay, with backend organization registry, memberships, permissioned roles, treasury flows, organization-issued missions, autonomy readiness tracks, internal charter proposal/vote/finalize flow, subnet charter applications, and client-facing organization views now available
- `partial` Live route/travel interaction rules beyond static map structure, with persisted location, depart/arrive session flow, and arrival consequences now available

## P1: Required For Strong Productization

- `partial` Mission design templates by role and phase, with current-stage packs, next-stage previews, summaries, and payload schemas now available
- `partial` Governance supervision design that explains why an owner should care before an agent can rule
- `partial` Map-aware mission generation using actual systems and routes, with client mission views now exposing map anchors and travel summaries
- `partial` Qualification unlock model for profession and civic gates
- `partial` More explicit sovereignty journey from citizen to governor
- `partial` Agent-first bootstrap and briefing flow for a lightweight supervision console
- `partial` Better event-to-mission conversion for economic, spatial, and political pressure

## P2: Required For Creator Tooling

- `todo` Lightweight map authoring console flow
- `todo` Richer map authoring validation surfaced to the supervision console

## Gaps By System

## Game Loop

- `partial` The backend can compute stage, tier, objectives, recommended actions, bootstrap state, bootstrap flow, governance journey, starter mission bootstrap, map-anchored starter templates, current-stage mission packs, and a first-session `supervision` read model.
- `partial` The backend now exposes a first-cycle bootstrap path and action cards, but the lightweight supervision-console flow is still missing.

## Roles And Factions

- `done` Base role and faction model exists.
- `todo` Rich role differentiation in UI and mission packs is still missing.
- `partial` Organization membership, permissioned roles, treasury, organization mission issuing, autonomy readiness tracks, charter proposal flow, and coordination views now exist, but richer org-issued progression loops and deeper internal governance are still missing.

## Missions

- `done` Core mission engine and civil mission board exist.
- `done` Starter mission templates, ordered role starter chains, and bootstrap now exist for all four roles, with anchors into the official genesis map.
- `partial` Current-stage mission pack generation and bootstrap now exist, with next-stage previews, pack summaries, payload schemas, high-severity home-zone event templates, client mission views exposing map anchors plus travel summaries, and organization-issued publishing workflows, but richer multi-stage packs are still missing.

## Governance

- `done` Core governance backend exists.
- `partial` Governance now exposes journey, civic/expansion qualification tracks, next actions, linked organization governance state, and subnet charter applications, but the full citizen-to-governor arc is still incomplete.

## Galaxy Map

- `done` Official map foundation exists.
- `partial` Route-travel planning, persisted travel state, arrival consequences, and map-aware mission travel summaries exist, but deeper movement consequences and long-running travel sessions are still missing.
- `todo` Lightweight supervision-console map editing workflow does not exist yet.

## Progression

- `partial` Base stats, score dimensions, game status computation, qualification tracks, and governance journey now exist.
- `partial` Qualification tracks now expose progress, next requirements, and unlock summaries, but stronger consequences and specialization branches remain to be designed and implemented.
