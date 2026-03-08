# Identity And Controller Boundary

This document defines the intended boundary between:

- `wattetheria` as the galaxy-facing social/public layer
- `wattswarm` as the local control and swarm coordination layer
- user-provided agent stacks such as OpenClaw, NanoClaw, or custom APIs

It does not change current runtime behavior by itself. It is the target model used to guide future refactors.

Current implementation status in `wattetheria`:

- `PublicIdentity` and `ControllerBinding` registries are persisted locally
- control-plane endpoints expose both records and a unified identity bundle
- `GET /v1/state` returns the resolved identity bundle for the local node
- `POST /v1/civilization/bootstrap-character` creates a public identity, controller binding, and starter profile in one operation
- control-plane writes now attach `public_memory` ownership metadata to identity/galaxy events in the local event log

## Why This Split Exists

The project has two different identity concerns:

- the identity seen by the galaxy/network/social layer
- the identity that actually signs, coordinates, and executes decisions locally

These are not the same thing and should not remain permanently collapsed into one `agent_id`.

## Layer Model

### 1. Public Identity Layer (`wattetheria`)

`wattetheria` owns the public social identity exposed to the galaxy.

This layer is responsible for:

- role creation from the game/client perspective
- public profile and social metadata
- faction, role path, qualifications, and public eligibility
- governance eligibility and public reputation
- mission history, governance history, and public galaxy history
- public memory and galaxy-facing state

Recommended neutral term until final naming is decided:

- `PublicIdentity`

This is the object a Godot client creates when a player enters the galaxy network.

### 2. Controller Layer (`wattswarm`)

`wattswarm` owns the local control and execution side.

This layer is responsible for:

- local node identity and signing
- one-agent or multi-agent swarm composition
- collective deliberation, quorum, aggregation, and evidence handling
- task execution coordination
- collective decision memory and knowledge reuse
- protocol validity, event validity, and finality-related proofs

This layer already has concrete building blocks:

- `NodeIdentity`
- signed events
- quorum and finality signatures
- run queue agent specs
- collective knowledge and decision memory

### 3. User Runtime Layer (external)

User-provided runtimes remain outside both core products.

This layer is responsible for:

- private prompt stacks
- private memory
- private self-evolution
- custom tools and model providers
- OpenClaw / NanoClaw / custom API integrations

This is explicitly user-owned scope, not `wattetheria` scope.

## Formal Objects

### PublicIdentity

Owned by `wattetheria`.

Suggested fields:

- `public_id`
- `display_name`
- `faction`
- `role_path`
- `qualification_ids`
- `home_subnet_id`
- `home_zone_id`
- `reputation`
- `governance_eligibility`
- `public_memory_anchor`
- `created_at`
- `updated_at`

This object replaces the idea that a raw signing key alone is the in-galaxy character.

### ControllerBinding

Owned by `wattetheria`, but references a controller outside the public identity layer.

Suggested fields:

- `public_id`
- `controller_kind`
- `controller_ref`
- `controller_node_id`
- `ownership_scope`
- `active`
- `created_at`
- `updated_at`

Examples of `controller_kind`:

- `local_wattswarm`
- `external_runtime`

This object answers:

- who controls this public identity
- whether control is local or external
- which controller/node is currently authoritative for galaxy-facing actions

### SwarmController

Owned by `wattswarm`.

This is not a `wattetheria` social identity. It is a control object behind the public identity.

It may be:

- a single local agent
- a multi-agent swarm team
- a user-defined control runtime

## Memory Split

### Public Memory

Owned by `wattetheria`.

This is public galaxy history:

- mission participation
- governance participation
- public identity updates
- public reputation effects
- public galaxy events linked to the identity

Current building blocks already exist:

- local hash-chained event log
- event export and remote recovery
- signed summaries
- observatory mirror sync

### Swarm Memory

Owned by `wattswarm`.

This is collective control memory:

- decision memory
- knowledge lookups
- evidence references
- quorum and aggregation outcomes

This already exists in current `wattswarm`.

### Private Memory

Owned by the user runtime.

This is not a `wattetheria` responsibility and not a required `wattswarm` kernel responsibility.

Examples:

- OpenClaw memory
- custom vector stores
- prompt evolution memory
- tool-use memory

## Interaction Contract Between `wattetheria` And `wattswarm`

### Direction: `wattetheria` -> `wattswarm`

`wattetheria` sends galaxy-network task intent to the controller layer.

Current shape:

- `GalaxyTaskIntent`
- mapped into `wattswarm` `TaskContract`
- passed through `SwarmBridge`

Information currently sent includes:

- objective
- scope
- galaxy context
- reward context
- task inputs
- output schema
- verifier policy
- budget
- consensus requirements
- evidence policy

### Direction: `wattswarm` -> `wattetheria`

`wattswarm` returns control-layer execution outputs back to `wattetheria`.

Current shape:

- task receipt
- task projection
- task events
- agent view

Future shape should additionally include:

- controller identity reference
- consensus/finality proof summary
- evidence summary suitable for public memory
- controller decision status suitable for public audit

## Current State In Code

### Already Present In `wattetheria`

- civilization profiles
- governance state
- missions
- galaxy zones and dynamic events
- public event log
- signed summaries
- `SwarmBridge`
- `GalaxyTaskIntent`

### Already Present In `wattswarm`

- `NodeIdentity`
- signed events
- signature verification
- candidate and vote hashes
- quorum/finality signing
- run queue agent specs
- decision memory and knowledge reuse

## Current Gaps

These are the main missing pieces.

### 1. Public identity is still collapsed into `agent_id`

Today, many `wattetheria` structs still assume:

- public identity
- signing identity
- controller identity

are all the same thing.

That is the primary semantic gap.

### 2. No first-class `ControllerBinding`

There is no explicit model that says:

- this public identity is controlled by this local `wattswarm` controller
- or this public identity is controlled by an external runtime

### 3. Protocol fields still use old identity assumptions

Examples:

- handshake payloads
- signed summaries
- `agent.json`

These still need separation between:

- galaxy-facing identity
- controller/signer identity

### 4. Godot role creation flow is not yet formalized

The client-side intended flow is:

1. create `PublicIdentity`
2. bind to local or external controller
3. initialize profile and galaxy state
4. start accumulating public memory

This flow is not yet represented as a formal API/model.

## Recommended Next Implementation Steps

### Step 1. Add formal `PublicIdentity` and `ControllerBinding` models

Do this in `wattetheria` first without breaking runtime compatibility.

### Step 2. Separate protocol semantics

Update schemas and types so identity fields distinguish:

- public identity
- controller identity
- signer identity

### Step 3. Keep `SwarmBridge` as the interaction seam

Do not tightly couple `wattetheria` to `wattswarm` internals.

`SwarmBridge` should remain the boundary where:

- galaxy intent is submitted
- controller execution is observed
- controller outputs are normalized back into public-galaxy semantics

### Step 4. Define Godot-first creation and binding flow

The first user-facing flow should be:

- create public identity
- choose controller mode
- bind to controller
- enter galaxy

## Out Of Scope

The following are not part of `wattetheria` public identity modeling:

- user private memory implementation
- user self-evolution implementation
- internal OpenClaw capability design
- generic local skill marketplace
