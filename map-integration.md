# CADCraft map integration plan

**Status:** proposed design and implementation plan  
**Prepared:** 2026-10-18  
**Source reviewed:** [`storytold/cadcraft`](https://github.com/storytold/cadcraft), `main`  
**Scope:** Optional street or satellite basemap behind the CAD drawing

## 1. Goal and agreed behavior

Add an optional map backdrop to CADCraft. The map helps place and inspect a drawing against its real-world location; it remains a read-only visual reference.

- Map modes: **Off**, **Street**, and **Satellite**. Allow opacity adjustment.
- CADCraft remains responsible for the drawing, CAD editing, selection, snapping, measurement, and view navigation.
- Pan and zoom keep the map registered to the CAD viewport. A map is never selectable, snappable, measurable, editable, or written as drawing geometry.
- Do not change DXF entities to implement the background. Preserve current drawing content and save behavior.
- The project CRS and georeferencing must be explicit. Never assume that arbitrary drawing coordinates are longitude/latitude or a particular projected CRS.
- Keep map providers replaceable. Street and satellite sources can have different providers and terms.

This carries forward the earlier LibreCAD discussion: a native CAD canvas overlay, not a consumer map browser embedded as a terms workaround. OpenStreetMap-derived street data and a separately licensed satellite source are viable provider categories. Google Maps would require its supported API, key/billing, attribution, and a terms review; browser embedding does not remove those requirements.

## 2. CADCraft source structure reviewed

The source is a Rust workspace (`Cargo.toml`: `crates/*`, `apps/*`, and `xtask`) with a thin, swappable `ui-egui` front end. Relevant locations on `main`:

| Concern | Existing location | Map integration point |
|---|---|---|
| Application/workspace wiring | `Cargo.toml`, `apps/cadcraft/Cargo.toml`, `apps/cadcraft/src/main.rs` | Add new workspace crates and inject platform tile services from the app layer. |
| CAD canvas and viewport transform | `crates/ui-egui/src/canvas.rs` (`Xf`, `CanvasState`, `ensure_list`, canvas paint/input) | Paint map imagery first, then the existing CAD display list and overlays. Reuse the current viewport transform so pan/zoom stay synchronized. |
| CAD drawing display list and rasterization | `crates/render/` | Keep basemaps out of the CAD display list and ordinary CAD export path in the first version. |
| View commands and session state | `crates/engine/src/cmd/view.rs`, `crates/engine/src/cmd/*`, `crates/engine/src/lib.rs` | Add command-driven map controls/state where they affect the document or view; preserve command-line/control-channel access. |
| UI commands, menus, and persisted UI preferences | `crates/ui-egui/src/menus.rs`, `crates/ui-egui/src/lib.rs` (`UiState`, `CadApp::prefs_json`, `load_prefs`) | Add map menu/palette controls and persist user-level display choices through the existing UI state path. |
| Geometric primitives | `crates/geom/` (`cadcraft-geom`) | Reuse f64 math for transforms; do not put network or UI code here. |
| Drawing/document model and file mapping | `crates/doc/`, `crates/io/` | Decide how per-drawing CRS/georeference metadata is saved without changing entities. |
| Layer and wasm checks | `AGENTS.md`, `cargo xtask layers`, `cargo xtask wasm`, `cargo xtask ci` | New dependencies and crates must obey layering and keep native and WASM builds working. |

The source instructions require user-visible behavior to be command-driven, keep egui dependencies at the UI layer, avoid panics, preserve wasm support, and check asset attribution. The plan follows those constraints.

## 3. Proposed architecture

### 3.1 Crate boundaries

Add a small, UI-independent `cadcraft-map` crate only after checking the repository's library ADR and shared `craftrules`:

- **`crates/map/` (`cadcraft-map`, proposed):** CRS identifiers, explicit georeference model, coordinate transforms, XYZ tile math, source metadata/attribution, cache policy, and provider traits. Pure Rust; no egui or application code.
- **Tile transport:** keep network access behind an injected tile-service interface. Initially implement the service at the application/UI boundary, with separate native and WASM implementations as required. Do not make the geometry or engine crates issue HTTP requests.
- **`crates/ui-egui/src/map/` (proposed):** map display state, asynchronous tile request coordination, bounded decoded-image cache, attribution display, and status/error reporting.
- **`crates/ui-egui/src/canvas.rs`:** compose the map below the CAD display list, grid, cursor, snap markers, selection feedback, and other CAD overlays. The map shares `Xf`/view state; it does not own a second independent camera.
- **Engine/document integration:** keep a typed map/georeference model behind a small API. Do not leak provider-specific fields into `cadcraft-geom` or mutate entities to hold map pixels.

Before implementation, inspect `plan/adr/0001-geometry-libraries.md`, current `plan/STATUS.md`, `plan/execution-plan.md`, and the relevant sections of `plan/architecture.md` in the local checkout. `plan/` is gitignored in this repo; commit the feature design under `docs/` if it should be shared.

### 3.2 Coordinate model

Use an explicit transform chain:

`drawing coordinates + drawing units → project CRS → WGS 84 longitude/latitude → tile CRS (normally Web Mercator)`

For Melbourne-area acceptance tests, include **GDA2020 / MGA zone 55 (EPSG:7855)**. Keep CRS and axis-order handling explicit. Use a maintained transform library only after validating its licence, native build requirements, wasm support, and availability under CADCraft's dependency policy. A native-only PROJ dependency is not acceptable if it breaks supported web builds.

A new or existing drawing without a configured CRS must show a clear “georeference required” state; do not guess. The initial georeference workflow should let the user set the CRS and register drawing coordinates to a known map location. More elaborate multi-point fitting can be a later feature.

### 3.3 Provider interface and policy

Model providers behind a provider-independent interface that returns tile bytes plus source identity, attribution, zoom limits, and source-specific cache rules. Start with one street provider suitable for development and internal evaluation. Add satellite imagery only after its provider and licence are selected.

- Show required attribution whenever map pixels are visible.
- Respect provider rate limits, attribution, API key/billing, storage, and display requirements.
- Do not scrape consumer map pages or assume that visible browser access permits embedding, bulk requests, or persistent caching.
- Make caching bounded and provider-policy-driven; disabling the map stops new requests. Do not promise offline maps unless the selected provider expressly allows that use.
- If no approved provider is configured, the CAD app remains fully usable with the map off and reports the map-source issue without crashing.

### 3.4 Persistence and DXF compatibility

Separate **user display preference** (default map mode, opacity, cache limit) from **drawing georeference** (CRS, anchor/transform, source settings if needed). Use the existing UI preference serialization for user-level settings.

Before coding, record an ADR for per-drawing persistence. Prefer a non-entity document metadata representation only if it round-trips safely through CADCraft's supported file formats and does not discard existing content. Otherwise use a clearly named sidecar file. Do not silently write provider credentials, tile images, or cache data into DXF. Add round-trip tests proving all existing DXF entities and unknown content remain unchanged.

## 4. Implementation phases

### Phase 0 — confirm contracts and dependencies

1. In a local checkout, read the private `plan/` status/execution/architecture files named above and the geometry-library ADR.
2. Trace `CanvasState`, `Xf`, canvas painter ordering, `CadApp::Services`, preference loading, and native/web app initialization.
3. Write ADRs for coordinate-transform dependency and georeference persistence. Validate native + wasm builds before committing to a dependency.
4. Confirm a street tile source and a licensed satellite candidate, with attribution and cache rules. Do not hard-code an unapproved endpoint.

**Exit:** dependency layering, metadata location, provider terms, and offline/error behavior are decided.

### Phase 1 — static basemap vertical slice

1. Add coordinate and tile-key types plus unit tests to `cadcraft-map`.
2. Add a provider-neutral tile-service boundary and a mock source for tests.
3. Render a small set of fetched/cached tiles behind the current drawing in the canvas, using the same viewport transform.
4. Add Off/Street controls, opacity, attribution, loading/error feedback, and a georeference-required state.
5. Ensure map imagery cannot enter selection, snap, measurement, undo, CAD display-list, or default export pipelines.

**Exit:** a locally georeferenced drawing remains aligned while pan/zoom changes; map-off makes no requests; no drawing entity changes.

### Phase 2 — georeference and project persistence

1. Add explicit CRS selection and drawing-to-CRS registration.
2. Implement and test EPSG:7855 ↔ WGS 84 ↔ tile-CRS transformations using the approved dependency.
3. Persist georeference metadata via the chosen document metadata or sidecar design.
4. Test open/save/reopen, missing or invalid metadata, corrupt sidecars, and legacy drawings.

**Exit:** registration survives reopen and file round-trips preserve CAD content.

### Phase 3 — production providers and caching

1. Implement the approved street source and its required attribution.
2. Add satellite mode only with an approved licensed source and terms review.
3. Add bounded memory/disk caching, expiry, cancellation, offline indication, and rate limiting based on each source's rules.
4. Keep API keys out of drawings, logs, source control, and exported files; use supported configuration/secret handling.

**Exit:** provider terms are met, failures are contained, and the selected sources work on supported platforms.

### Phase 4 — commands, documentation, and release

1. Expose map settings through CADCraft command/menu conventions so the same behavior is reachable from UI and automation where appropriate.
2. Document CRS selection, provider setup, attribution, cache behavior, and limits.
3. Run `cargo xtask ci`, including `cargo xtask layers` and `cargo xtask wasm`; visually inspect canvas screenshots at several zoom levels and on a large drawing.
4. Update relevant `docs/` status and roadmap material when the feature is prioritized and accepted.

## 5. Acceptance criteria

- Off/Street/Satellite mode and opacity behave predictably; attribution remains legible over the map.
- A map is always behind CAD geometry and does not participate in CAD editing, snapping, selection, measurement, or undo.
- Map and drawing remain spatially registered through pan, zoom, window zoom, and viewport resize.
- CRS is explicit; unknown CRS never produces a silently misleading overlay. EPSG:7855 has a known-point and round-trip test.
- Map failures, missing network, invalid provider response, and bad saved metadata do not crash or corrupt the drawing.
- Saving/reopening preserves all CAD entities and unknown file content; map metadata follows the agreed persistence model.
- Provider attribution and API/storage/rate-limit rules are respected; cache behavior is source-specific.
- Native and wasm checks pass; no UI dependency leaks below `ui-egui` and no network dependency leaks into geometry/rendering.
- CAD-only export remains unchanged unless a future explicit “include map” export is designed and provider terms permit it.

## 6. Main risks and decisions still open

| Decision/risk | Why it matters | Resolution gate |
|---|---|---|
| Coordinate transform crate | Accuracy, platform packaging, wasm support, and licence | Phase 0 ADR and native/wasm spike |
| Per-drawing metadata format | DXF round-trip safety and project portability | Phase 0 ADR; no entity changes |
| Street tile source | Attribution, rate limits, reliability, cache policy | Select source before production requests |
| Satellite source | Commercial/internal-use licensing and cost | Separate selection and terms review |
| Performance | Image decoding, network latency, large canvases | Async loading, bounded cache, cancellation, profiling |
| Drawing intent | CAD units alone do not establish location or CRS | Require explicit user georeferencing |

## 7. Repository references

- [CADCraft source repository](https://github.com/storytold/cadcraft)
- [`AGENTS.md`](https://github.com/storytold/cadcraft/blob/main/AGENTS.md)
- [`crates/ui-egui/src/canvas.rs`](https://github.com/storytold/cadcraft/blob/main/crates/ui-egui/src/canvas.rs)
- [`crates/ui-egui/src/lib.rs`](https://github.com/storytold/cadcraft/blob/main/crates/ui-egui/src/lib.rs)
- [`crates/engine/src/cmd/view.rs`](https://github.com/storytold/cadcraft/blob/main/crates/engine/src/cmd/view.rs)
- [`crates/render/`](https://github.com/storytold/cadcraft/tree/main/crates/render)
