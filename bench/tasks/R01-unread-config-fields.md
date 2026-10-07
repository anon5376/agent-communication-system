---
id: R01
kind: research
role: research
title: List the declared config fields that are never read outside validateConfig
mini: true
truth: truth/R01.json
---

## Brief
In `src/config.ts`, the interfaces `BusConstraints`, `AgentPermissions`, `AgentDefinition`, `RolePolicy` and `HarnessFeatureSet` declare configuration fields. Find every field of these five interfaces that is never read anywhere under `src/` outside the body of the function `validateConfig`.

Definitions:
- A field is one property signature declared directly in one of those five interfaces. Fields of other interfaces (for example `HarnessDefinition`, `ModelDefinition`, `RoutingWeights`) are out of scope.
- A read is a non-write reference to that exact property of that exact type: property access, element access with a literal key, or destructuring. Inherited access counts (for example through a type that extends one of these interfaces).
- These are not reads: the declaration itself; assignments to the property; object literals that set the property (they are writes); references inside the body of `validateConfig`; anything outside `src/` (tests, dist, docs).
- Same-named properties on other types do not count. Resolve each reference by type, not by text match: a property called `canDelegate`, `authority`, `description` or `maxRetries` on some other type is not a read of the config field.
- Copying a whole object (spread, or assigning the parent object) is not a read of its individual fields.

Answer form: the list of unread fields, each named `Interface.field` exactly as declared (for example `RolePolicy.minimumCapability`). For each listed field, give a claim citing its declaration line, and for any field that is read only inside `validateConfig`, cite that line too.

## Acceptance
- `items` holds every in-scope field with no read outside `validateConfig`, each as `Interface.field`, and nothing else.
- Each item has a claim with `path:line` evidence for its declaration in `src/config.ts`. Fields read only inside `validateConfig` also cite that read.
- Any field the answer judged borderline (for example one that is only written, or only spread) appears in `unresolved` or in a claim explaining the call.
