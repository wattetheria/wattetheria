# 06. Progression And Influence

## Principle

Wattetheria should not use a traditional numeric level grind as its primary progression fantasy.

Instead, progression should answer:

- how much can this public identity operate
- how much can this public identity influence
- how much can this public identity govern
- how much can this public identity expand

## Current Core Stats

The current base stats are:

- `watt`
- `power`
- `reputation`
- `capacity`

These are the right low-level foundation.

## Current Civilization Scores

The current score dimensions are:

- `wealth`
- `power`
- `security`
- `trade`
- `culture`
- `total_influence`

These provide a strong multidimensional progression model.

## Productized Progression Layer

On top of the raw stats and scores, the first gameplay layer should expose:

- `stage`
  - survival
  - foothold
  - influence
  - expansion

- `tier`
  - initiate
  - specialist
  - coordinator
  - sovereign

- `objectives`
  - next clear actions

- `recommended_actions`
  - role-aware guidance

This layer is now present in the backend `game` module.

## Why This Is Better Than Traditional Levels

It keeps the game aligned with the galaxy-society fantasy.

Public identities are not just “leveling up.” They are:

- earning position
- building civic leverage
- shaping routes and planets
- expanding influence

## Future Progression Extensions

Likely future additions:

- qualification unlocks
- role specialization branches
- governance-grade unlocks
- organization-linked progression beyond membership, readiness, shared mission visibility, and autonomy readiness tracks
- map-expansion eligibility

## Current Backend Coverage

Already implemented in code:

- base stats
- civilization score computation
- game stage and tier
- objectives and recommended actions
- home-anchor resolution from official map state
- structured qualification tracks with progress, next requirements, and unlock summaries

Still missing:

- profession-grade progression tracks
- stronger ties between progression and governance permissions
- stronger ties between progression and route control responsibilities
