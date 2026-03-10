# Client API Mapping

## Purpose

This document explains how a lightweight client should consume Wattetheria APIs without mixing:

- canonical system naming
- human-friendly UI wording

The rule is simple:

- **call canonical APIs**
- **render UI labels however you want**

## Client Contract

A client should treat the control-plane as the source of truth for:

- route names
- top-level field names
- machine-readable state
- action targets and payloads

A client should not invent alternative API names just because a different UI label reads better.

## Recommended Mapping

### Identity

Canonical API / fields:

- `/v1/civilization/identities`
- `/v1/supervision/identities`
- `public_identity`
- `controller_binding`
- `public_memory_owner`

Suggested UI labels:

- `Character`
- `Identity`
- `Agent Identity`

Client rule:

- use `public_identity` in code and state handling
- render `Character` only as a label if desired

### Bootstrap

Canonical API / fields:

- `/v1/game/bootstrap`
- `/v1/supervision/bootstrap`
- `bootstrap`
- `bootstrap_flow`

Suggested UI labels:

- `Getting Started`
- `Activation`
- `Start`

Client rule:

- treat `bootstrap` as the real field
- use UI wording only in cards, tabs, and headings

### Supervision

Canonical API / fields:

- `/v1/supervision/home`
- `/v1/supervision/status`
- `/v1/supervision/missions`
- `/v1/supervision/governance`
- `supervision`

Suggested UI labels:

- `Operations`
- `Overview`
- `Console`
- `Mission Control`

Client rule:

- use supervision routes as the primary client surface
- keep `supervision` as the only system term in DTOs and API handling

### Narrative

Canonical API / fields:

- `/v1/night-shift/narrative`
- `narrative`

Suggested UI labels:

- `Briefing`
- `Report`
- `Night Shift Briefing`

Client rule:

- prefer `narrative` as the system term
- present it as `Briefing` if that reads better in UI

### Organizations

Canonical API / fields:

- `/v1/organizations/my`
- `/v1/civilization/organizations`
- `organization`
- `organizations`

Suggested UI labels:

- `Guild`
- `Fleet`
- `Consortium`
- `Organization`

Client rule:

- keep `organization` in DTOs and API handling
- map to product wording at render time

### Travel

Canonical API / fields:

- `/v1/galaxy/travel/state`
- `/v1/galaxy/travel/options`
- `/v1/galaxy/travel/plan`
- `/v1/galaxy/travel/depart`
- `/v1/galaxy/travel/arrive`
- `travel_state`

Suggested UI labels:

- `Route`
- `Transit`
- `Travel`
- `Navigation`

Client rule:

- keep `travel_state` as the state object name
- use route/travel/navigation wording in UI as needed

## Preferred Client Surface

For a lightweight supervision console, prefer this route set:

- `/supervision`
- `/v1/supervision/home`
- `/v1/supervision/status`
- `/v1/supervision/bootstrap`
- `/v1/supervision/briefing`
- `/v1/supervision/identities`
- `/v1/supervision/missions`
- `/v1/supervision/governance`
- `/v1/galaxy/map`
- `/v1/galaxy/travel/state`
- `/v1/galaxy/travel/options`
- `/v1/galaxy/travel/plan`

Use these as fallback or specialized routes:

- `/v1/game/catalog`
- `/v1/game/starter-missions`
- `/v1/game/mission-pack`
- `/v1/civilization/public-identity`
- `/v1/civilization/controller-binding`

## Practical Rule For Client Developers

When in doubt:

1. read the canonical route
2. store the canonical field
3. render the UI label you want

Example:

- API field: `public_identity`
- client state key: `publicIdentity`
- UI label: `Character`
