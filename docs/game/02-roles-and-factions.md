# 02. Roles And Factions

## Roles

The first release uses four roles. They are social and operational specializations, not traditional RPG combat classes.

### Operator

Focus:

- infrastructure
- upkeep
- logistics
- throughput

Gameplay fantasy:

- keep systems working
- stabilize home regions
- turn reliability into influence

Best-fit content:

- infrastructure missions
- route maintenance
- production and maintenance chains

### Broker

Focus:

- trade
- liquidity
- transport
- market leverage

Gameplay fantasy:

- connect disconnected regions
- profit from movement and coordination
- shape the trade layer of the galaxy

Best-fit content:

- trade missions
- supply balancing
- frontier exchange activity

### Enforcer

Focus:

- patrol
- escort
- route security
- coercive pressure

Gameplay fantasy:

- keep routes safe or make them dangerous
- transform risk into authority
- influence security and sovereignty outcomes

Best-fit content:

- security missions
- frontier response
- route control and pressure events

### Artificer

Focus:

- construction
- cultural gravity
- identity creation
- symbolic influence

Gameplay fantasy:

- make locations matter
- build recognizable places and narratives
- turn creative work into durable galaxy influence

Best-fit content:

- culture missions
- civic branding
- future landmark and map-design pathways

## Factions

Factions are political cultures, not hard class locks.

### Order

Values:

- stability
- enforceable legitimacy
- infrastructure-first governance

### Freeport

Values:

- open exchange
- corridor neutrality
- market-first coordination

### Raider

Values:

- frontier pressure
- opportunistic control
- risk-first politics

## Role And Faction Interaction

Roles describe what the player is best at doing.

Factions describe how the player tends to justify power.

Example combinations:

- Order + Operator
  - system stabilizer
- Freeport + Broker
  - exchange architect
- Raider + Enforcer
  - pressure specialist
- Freeport + Artificer
  - culture-branding force

## Current Backend Coverage

Already implemented in code:

- factions: `order`, `freeport`, `raider`
- roles: `operator`, `broker`, `enforcer`, `artificer`
- strategy profiles: `conservative`, `balanced`, `aggressive`
- profile persistence and role/faction-aware mission filtering

Still missing:

- role-specific UI onboarding
- role-specific starter objective chains with ordered backend steps
- faction-specific governance modifiers
- faction-specific narrative/event flavor
