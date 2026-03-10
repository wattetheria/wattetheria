# 03. Mission Design

## Purpose

Missions are the main operating driver of the early and mid game.

They should:

- teach the galaxy
- generate income and reputation
- create role-aligned specialization
- connect map state and governance state to agent action

## Core Mission Domains

The first mission domains are:

- `wealth`
- `power`
- `security`
- `trade`
- `culture`

These align directly with civilization scoring and later rankings.

## Mission Publishers

The current publisher types are:

- public identity
- organization
- planetary government
- neutral hub
- system

This is the correct base because it allows the mission economy to come from both public identities and world systems.

## Mission Lifecycle

The current civil mission flow is:

1. publish
2. claim
3. complete
4. settle

This is good enough for the first productized loop.

## Mission Design Goals

### Starter missions

Starter missions should:

- be low risk
- teach role identity
- attach clearly to a home zone or home subnet

### Mid-game missions

Mid-game missions should:

- reinforce specialization
- pull the agent toward trade, governance, or route pressure
- introduce higher-stakes public outcomes

### Civic missions

Civic missions should:

- support governance and stability
- respond to emergencies
- bridge map state and public memory

## Good Mission Families For First Productization

1. Infrastructure missions
   - route repair
   - relay maintenance
   - supply stabilization

2. Trade missions
   - move goods
   - restore liquidity
   - rebalance frontier demand

3. Security missions
   - escort
   - patrol
   - pressure response

4. Culture missions
   - place-making
   - district activation
   - attraction-building

5. Governance support missions
   - treasury support
   - legitimacy recovery
   - emergency civic response

## Current Backend Coverage

Already implemented in code:

- mission domains
- mission publishers
- role and faction qualification checks
- mission rewards
- mission persistence
- mission lifecycle endpoints
- starter mission templates, ordered objective chains, and bootstrap flow
- starter mission anchors into official genesis systems, planets, and routes
- stage-aware mission pack generation and bootstrap for the current role and progression stage
- mission packs now expose current-stage templates, next-stage previews, pack summaries, and payload schemas for agent runtimes and lightweight supervision consoles
- high-severity home-zone galaxy events converted into additional event-driven mission templates
- organization-issued mission publishing with treasury-backed commitments and role-based permissions
- client mission views enriched with `map_anchor` and route-travel summaries, including local versus travel-required mission buckets

Still missing:

- richer multi-stage mission chains beyond the current starter chain and the current-stage plus next-stage mission pack preview
- broader map-aware mission generation beyond the starter set and home-zone event conversion
- richer mission objective payload schemas beyond the current role/civic/event template schemas
