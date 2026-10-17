# Map integration: Phase 0 implementation plan

**Status:** Phase 1 OpenStreetMap tile transport and desktop sidecars integrated; web sidecar import/export remains future work
**Date:** 2026-10-18

This turns the design in the local `map-integration.md` into an implementation sequence. It adds a
read-only map backdrop only: never a CAD entity, snap target, selection target, measurement input,
undo item, or default export layer.

## Decisions recorded

- Coordinate transforms: [ADR 0001](adr/0001-map-coordinate-transforms.md).
- Per-drawing registration: [ADR 0002](adr/0002-map-georeference-persistence.md).
- Default state: map mode is Off and an ungeoreferenced drawing makes no tile requests.
- Tile protocol: raster XYZ PNG/JPEG only in the first slice; all tile indices use checked integer
  arithmetic and zoom is capped at a provider-declared maximum.
- Street provider: [OpenStreetMap Standard](adr/0003-map-street-provider.md) is the initial
  built-in Street provider. Its adapter enforces visible attribution, identifying User-Agent,
  cache headers, and no prefetch/offline download. Self-hosted or approved hosted
  OpenStreetMap-derived providers remain configuration-selectable alternatives.
- Satellite provider: no built-in provider or endpoint in the first slice. A configured provider
  must declare attribution, zoom bounds, cache rules, and whether satellite mode is licensed.
- Commercial-provider adapter target: MapTiler Cloud is the first integration candidate, not an
  embedded credential. It requires the user's own key, visible attribution, direct client requests,
  and its end-user cache restrictions. Production activation requires an account owner to approve
  the selected plan and terms.

## Exact ownership and seams

| Area | Planned change | Why |
|---|---|---|
| `Cargo.toml` | Register `cadcraft-map`; add only approved pure-Rust dependencies. | Keeps shared versions and workspace checks intact. |
| `crates/map/` | CRS, registration, Web-Mercator/XYZ math, provider metadata/registry, cache-policy types, tests. | UI/network independent and WASM-safe. |
| `crates/io/` | Bounded `<drawing>.cadcraft-map.json` read/write API plus tests. | Sidecar lifecycle belongs with drawing file handling. |
| `crates/engine/` | Command specs for map state and registration; no networking. | Menus, command line, scripts, control channel, and MCP use one behavior. |
| `crates/ui-egui/src/map.rs`, `map_tiles.rs` | Tile request coordinator, decode boundary, bounded in-memory cache, native disk cache, attribution painter. | UI owns visual lifecycle and asynchronous repaint. |
| `crates/ui-egui/src/canvas.rs` | Paint map after canvas/paper background and before `draw_grid`, viewport content, and CAD display lists. | Maintains the required visual and interaction order. |
| `crates/ui-egui/src/map_tiles.rs` | Cross-platform HTTP transport; native cache and browser HTTP cache use their respective platforms. | No HTTP dependency leaks below the UI boundary. |

`canvas.rs` currently creates `Xf` from the CAD viewport and calls `ensure_list` before drawing the
CAD display list. The map painter consumes that same `Xf`; it must not create a second pan/zoom
camera. It receives no pointer events and exposes no geometry to engine selection or snapping.

## Command contract

Phase 1 introduces non-interactive JSON forms with finite-number validation:

| Command | Parameters | Result |
|---|---|---|
| `map.mode` | `{ mode: "off" | "street" | "satellite" }` | Changes the view mode; `off` cancels/avoids requests. |
| `map.opacity` | `{ opacity: 0.0..1.0 }` | Changes map compositing opacity. |
| `map.georef.set` | `{ crs, drawingAnchor: [x,y], projectAnchor: [x,y], rotation?, scale? }` | Validates and stores the registration for sidecar save. |
| `map.georef.clear` | `{}` | Removes the in-memory registration and requests sidecar removal on save. |
| `map.status` | `{}` | Returns configured provider, map mode, registration state, and a non-secret error/status. |

Interactive UI invocation may guide registration, but `engine.execute` never opens a dialog. UI
preference serialization owns the default mode and opacity; the sidecar owns only registration.

## Implementation arcs

1. **Map core and tests — completed 2026-10-18.** Add `cadcraft-map` with `CrsId`, finite
   coordinates, `Registration`, `TileId`, tile-bound calculation, anti-meridian handling, zoom
   limits, and hostile-input tests. No network, image, UI, or provider endpoint.
2. **Transform spike — completed 2026-10-18.** Add the pinned `proj4rs` adapter behind a feature used by the new crate.
   Prove `EPSG:7855 <-> WGS 84 <-> EPSG:3857` with a Melbourne known point, round-trip tolerance,
   invalid-coordinate tests, `cargo test`, and `cargo xtask wasm`. Reject the dependency if this
   fails; do not substitute native PROJ.
3. **Sidecar and commands — desktop hooks completed 2026-10-18.** Add the schema to `cadcraft-io`, load/save hooks in the desktop and
   web hosts where supported, and engine commands with unit tests. Prove an ordinary DXF byte-level
   entity/unknown-content round trip is unaffected.
4. **Mock-backed rendering slice — completed 2026-10-18, then replaced by live tiles.** Add `ui-egui::map` with a deterministic mock tile service and
   a bounded decoded image cache. Compose tiles using `Xf` underneath grid/CAD content, draw
   attribution, loading, georeference-required, and error states. Verify no map primitives reach a
   `DisplayList` or `cadcraft-render` export.
5. **Transport and provider activation — native/WASM implementation in verification.** Implement the OpenStreetMap Standard adapter with
   queued-request cancellation, request de-duplication, bounded concurrency, conditional requests, and cache
   header handling at the application boundary. Keep the provider registry configuration-driven
   so self-hosted or approved hosted OpenStreetMap-derived sources can be added without changing
   georeferencing or canvas code. Satellite is a separate feature gate.

## Required verification

- Unit: XYZ boundaries, non-finite numbers, out-of-range zoom/tile values, CRS axis order,
  registration inverse, and corrupt/oversized sidecars never panic.
- Integration: map off makes zero requests; missing registration makes zero requests; map pixels
  never affect selection, snap, measure, undo, drawing revision, or CAD-only exports.
- Visual: screenshots at multiple zooms/pans, resize, paper/model space, and tiled viewports show
  map below grid and entities; attribution stays legible.
- Compatibility: native and `wasm32-unknown-unknown` builds; `cargo xtask layers`, `cargo xtask
  wasm`, then `cargo xtask ci` before each feature-arc commit.

## Phase 1 exit checklist

- [x] The OpenStreetMap Standard endpoint is isolated in its provider adapter; no secret is
  hard-coded and alternative providers do not require a CRS/canvas change.
- [x] The map state can be driven through commands and UI without dialogs in programmatic calls.
- [x] The sidecar is optional, bounded, versioned, and has failure-path tests.
- [x] A mock tile is correctly registered while CAD pan/zoom changes.
- [ ] CAD drawing contents and ordinary export are unchanged.
- [ ] Native/WASM/layer checks pass.

## Provider sources

- [OpenStreetMap tile usage policy](https://operations.osmfoundation.org/policies/tiles/)
- [MapTiler Cloud terms](https://www.maptiler.com/terms/cloud/)
