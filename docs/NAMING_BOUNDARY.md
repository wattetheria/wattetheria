# Naming Boundary

## Purpose

Wattetheria uses two naming layers:

- **canonical system naming**
- **UI presentation naming**

These layers must not be mixed casually.

## Canonical System Naming

Canonical system naming is used for:

- Rust types
- persisted state
- protocol and event names
- control-plane primary API fields
- internal service naming
- cross-crate boundaries

Canonical naming should describe what the system actually is, not what is easiest for a human to read in a game UI.

Preferred canonical terms:

- `public_identity`
- `controller_binding`
- `controller_id`
- `public_id`
- `bootstrap`
- `bootstrap_flow`
- `supervision`
- `narrative`
- `organization`
- `mission`
- `travel_state`

Avoid introducing new canonical terms when they only restate an existing one in more game-like language.

## UI Presentation Naming

UI presentation naming is for:

- lightweight supervision console labels
- future client labels
- cards, headings, summaries, and tooltips
- human-readable docs that explain the product conceptually

UI naming may use more human-friendly product words when they improve comprehension.

Examples:

- `public_identity` may be presented as `Character`
- `bootstrap` may be presented as `Start` or `Activation`
- `supervision` may be presented as `Operations`
- `organization` may be presented as `Guild`, `Fleet`, or `Consortium`
- `narrative` may be presented as `Briefing`

UI naming must not replace canonical naming in the system model.

## Mapping Rule

The rule is:

- **system stays canonical**
- **UI may translate**

This means:

- APIs should expose one primary canonical field or route
- UI code may map canonical fields into user-facing labels
- docs should clearly distinguish canonical names from presentation terms

## Current Canonical Choices

Use these as the primary system names:

- `public_identity` instead of `character`
- `bootstrap` instead of `onboarding`
- `supervision` instead of `experience`
- `narrative` instead of `humanized`
- `identities` instead of `characters`
- `supervision_home` instead of `dashboard_home`

## Compatibility Rule

Canonical names are the only names allowed in active system contracts.

UI wording may vary for readability, but DTOs, storage, events, and internal APIs should not keep parallel alias names.

## Engineering Rule

When adding or changing code:

- choose canonical names first
- add UI-friendly wording only at the presentation layer
- do not add duplicate fields just to carry both product wording and system wording
- if a UI needs friendlier language, translate it in the UI layer or in a dedicated presentation DTO
