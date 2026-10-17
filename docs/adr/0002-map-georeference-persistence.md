# ADR 0002: map registrations use a versioned sidecar

**Status:** accepted for the map Phase 0 design
**Date:** 2026-10-18

## Context

A map registration belongs to a drawing, while display mode and opacity are user preferences.
Neither tile pixels nor provider credentials belong in a DXF. The existing DXF path deliberately
preserves entities, foreign objects, and unknown content; adding a map entity or embedding cache
data would make a visual reference part of drawing geometry and risk round-trip compatibility.

## Decision

Persist only provider-neutral drawing registration in a UTF-8 JSON sidecar next to a saved drawing:

`<drawing filename>.cadcraft-map.json`

The schema starts with `schemaVersion: 1` and contains:

- the drawing CRS identifier and drawing-unit declaration;
- a validated affine drawing-to-project registration, initially one anchor plus uniform scale and
  rotation; and
- no provider URL, API key, tile bytes, cache index, display mode, or opacity.

The sidecar is read after the drawing opens. Missing sidecars mean "not georeferenced". Invalid,
unknown-version, oversized, non-finite, or unreadable sidecars are ignored after a clear warning;
they never block opening a drawing or mutate it. Saving writes the sidecar atomically after the
drawing write succeeds. A failed sidecar write reports an error and preserves the prior sidecar.

For unsaved drawings, registration remains in memory. Save As creates a sidecar at the new drawing
path only after explicit successful drawing save. Rename/move handling is documented as an explicit
user action for the first release: keep the sidecar with its drawing.

## Consequences

- DXF, DWG, SVG, PDF, and PNG pipelines remain unchanged in Phase 1; CAD-only exports never
  contain map imagery.
- `cadcraft-io` owns bounded sidecar parsing/writing and depends on `cadcraft-map`; the document
  entity model remains unchanged.
- Future embedded CADCraft metadata requires a separate ADR and raw-DXF round-trip proof.
