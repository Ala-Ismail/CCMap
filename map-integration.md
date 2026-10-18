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
- For the common survey-DWG workflow, the drawing already contains coordinates in its correct MGA zone. The user selects/confirms that zone, and CADCraft uses the existing drawing coordinates unchanged: do not assume `(0,0)` is a survey origin and do not apply an origin shift, scale, rotation, or other registration transform.
- Anchor-based correction/alignment is optional and separate from CRS selection. It is not required for an already-correct MGA survey drawing; users who need it can explicitly align map registration without modifying drawing geometry.
- Keep map providers replaceable. Street and satellite sources can have different providers and terms. Satellite imagery may use Google Maps Platform Map Tiles API hybrid tiles (satellite imagery with Google's roadmap layer composited into each tile) with a user-supplied (BYOK) key, or Esri's current World Imagery with labels. Esri's documented integration uses World Imagery map tiles as the raster base and `arcgis/imagery/labels` from the Static Basemap Tiles service as a transparent overlay. Esri World Imagery is the current mosaic, not the historical Wayback archive; imagery date, resolution, and source vary by location. These sources remain separate alternatives and are not mixed with OpenStreetMap imagery.

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

For an already-correct MGA drawing, use its existing coordinates directly as the project coordinates after the user selects the CRS. No drawing-to-CRS registration transform is applied. For drawings that need correction, the optional anchor-alignment command updates only the map registration, using the transform chain:

`drawing coordinates + drawing units → project CRS → WGS 84 longitude/latitude → tile CRS (normally Web Mercator)`

For Melbourne-area acceptance tests, include **GDA2020 / MGA zone 55 (EPSG:7855)**. Keep CRS and axis-order handling explicit. Use a maintained transform library only after validating its licence, native build requirements, wasm support, and availability under CADCraft's dependency policy. A native-only PROJ dependency is not acceptable if it breaks supported web builds.

A new or existing drawing without a configured CRS must show a clear CRS-required state; do not guess. For survey drawings, the initial workflow should let the user select the correct MGA zone and use its coordinates as-is. The separate anchor workflow is available for drawings that need registration to known map coordinates; more elaborate multi-point fitting remains out of scope.

### 3.3 Provider interface and policy

Model providers behind a provider-independent interface that returns tile bytes plus source identity, attribution, zoom limits, and source-specific cache rules. Esri's current World Imagery provider uses two aligned XYZ raster sources: the `World_Imagery/MapServer/tile/{z}/{y}/{x}` imagery base and the Static Basemap Tiles `arcgis/imagery/labels/static/tile/{z}/{y}/{x}` overlay. Both receive the user-supplied ArcGIS access token. This is Esri's documented third-party integration pattern; it avoids scraping Wayback or embedding Esri's viewer. Keep their textures as independent layers so labels stay composited over imagery.

- Show Esri and all applicable imagery/label-provider credits while either layer is visible. Use service metadata/acknowledgments where available and do not omit provider credits. Esri identifies World Imagery as licensed under its Master Agreement and directs users to the applicable Terms of Use. Its World Imagery (for Export) item is specifically for offline exports and is not intended for live tile display; do not use it as the live source or infer offline/persistent storage rights for the regular basemap. Follow the Static Basemap Tiles service usage terms and usage metering for the user's ArcGIS account.
- Start conservatively: request only visible tiles and use a bounded in-memory cache unless the current service terms explicitly allow more. Do not promise offline maps or persist Esri tiles without confirming permission.
- ArcGIS API-key credentials are BYOK and memory-only by default. Desktop users may explicitly opt in to storing the key in their current OS user's credential store; web builds remain session-only. The World Imagery MapServer endpoint uses Esri's documented `token` query parameter; the Static Basemap Tiles label endpoint uses an `Authorization: Bearer` header. The key needs both `premium:user:basemaps` for World Imagery and `premium:user:staticbasemaptiles` for labels. Never store the key in the drawing, UI preference JSON, logs, source control, or export. Explain that service usage may be billed to the account that owns the credential.

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
4. Confirm a street tile source and licensed satellite candidates (Google hybrid and optional Esri current World Imagery via WMTS), with attribution and cache rules. Do not hard-code an unapproved endpoint.

**Exit:** dependency layering, metadata location, provider terms, and offline/error behavior are decided.

### Phase 1 — static basemap vertical slice

1. Add coordinate and tile-key types plus unit tests to `cadcraft-map`.
2. Add a provider-neutral tile-service boundary and a mock source for tests.
3. Render a small set of fetched/cached tiles behind the current drawing in the canvas, using the same viewport transform.
4. Add Off/Street controls, opacity, attribution, loading/error feedback, and a georeference-required state.
5. Ensure map imagery cannot enter selection, snap, measurement, undo, CAD display-list, or default export pipelines.

**Exit:** a locally georeferenced drawing remains aligned while pan/zoom changes; map-off makes no requests; no drawing entity changes.

### Phase 2 — georeference and project persistence

1. Add explicit CRS selection through a small in-app dialog. The initial Australian catalogue includes GDA2020 / MGA zones 46–56 (EPSG:7846–7856) and GDA94 / MGA zones 46–56 (EPSG:28346–28356), alongside the general WGS 84 and Web Mercator options. Group these by datum and show each zone and EPSG code; never infer the datum or zone from drawing coordinates. For an already-correct MGA survey drawing, selecting the CRS is sufficient: preserve all drawing coordinates unchanged, with no assumed origin or implicit transform.
2. Provide this dialog through the same `map.georef.set` command used by automation. Keep it focused on CRS selection. Do not include anchor picking, coordinate registration, scale/rotation correction, or other alignment controls in this phase.
3. Implement and test each offered MGA CRS ↔ WGS 84 ↔ tile-CRS transformation using the approved dependency, including known-point and round-trip coverage for GDA2020 / MGA zone 55 (EPSG:7855).
4. Persist the selected CRS via the chosen document metadata or sidecar design; CRS-only selection must preserve drawing coordinates unchanged.
5. Test the dialog and command path, open/save/reopen, missing or invalid CRS metadata, corrupt sidecars, and legacy drawings.

**Exit:** selected CRS survives reopen and file round-trips preserve CAD content, with survey coordinates unchanged.

### Phase 3 — production providers and caching

1. Implement the approved street source and its required attribution.
2. Implement Google hybrid mode through Map Tiles API session tokens (`satellite` map type, `layerRoadmap`, `overlay: false`), requesting only visible tiles and viewport copyright metadata. Show the Google Maps mark/text plus the full copyright returned for the current viewport.
3. Keep Google imagery in a bounded memory-only texture cache; do not prefetch, create an offline cache, or persist tile bytes. Keep OSM cache policy independent.
4. BYOK keys are user-supplied and kept in process memory by default. Desktop users may explicitly opt in to saving the key in their current OS user's credential store (Windows Credential Manager, macOS Keychain, or Secret Service on supported Linux/BSD desktops). Never serialize keys or session tokens into drawings, profile JSON, logs, source control, or exports. Web builds remain session-only. Warn that Google bills tile requests and that users should restrict the key to Map Tiles API.
5. Implement Esri current World Imagery with labels as a separate satellite mode: request the World Imagery raster base and the Static Basemap Tiles `arcgis/imagery/labels` overlay for visible tiles, using the user's ArcGIS access token. Keep both layers in bounded memory-only caches, show their Esri/data attributions, and never bundle or persist imagery. No Wayback catalog or date selector is involved.

**Exit:** provider terms are met, failures are contained, and the selected sources work on supported platforms.

### Phase 4 — commands, documentation, and release

1. Expose map settings through CADCraft command/menu conventions so the same behavior is reachable from UI and automation where appropriate.
2. Document CRS selection, provider setup, attribution, cache behavior, and limits.
3. Run `cargo xtask ci`, including `cargo xtask layers` and `cargo xtask wasm`; visually inspect canvas screenshots at several zoom levels and on a large drawing.
4. Update relevant `docs/` status and roadmap material when the feature is prioritized and accepted.

### Optional anchor-based alignment

Available separately from CRS selection through `MAP.GEOREF.ALIGN` / View > Map > Align Drawing to Map. It requires a CRS to have been selected first and never changes drawing entities. The interactive flow accepts a drawing anchor picked or typed in CAD coordinates and its matching map location as latitude,longitude; CADCraft converts that WGS 84 point into the selected project CRS. Programmatic callers may instead provide projected easting,northing values. Pressing Enter after the first pair applies translation only; supplying a second matched pair derives uniform scale and rotation. The command validates geographic bounds and that matched points define a finite, non-degenerate transform, reports the resulting CRS/scale/rotation, and persists the registration using the existing sidecar path. CRS-only MGA selection remains unchanged and continues to preserve already-correct survey coordinates.

## 5. Acceptance criteria

- Off/Street/Google Satellite/Esri Satellite-with-Labels modes and opacity behave predictably; each source's attribution remains legible over the map. Esri uses current World Imagery and does not expose Wayback historical imagery/date selection.
- A map is always behind CAD geometry and does not participate in CAD editing, snapping, selection, measurement, or undo.
- Map and drawing remain spatially registered through pan, zoom, window zoom, and viewport resize.
- CRS is explicit; unknown CRS never produces a silently misleading overlay. For an already-correct MGA drawing, selecting the zone uses its coordinates unchanged and never assumes `(0,0)` is the origin. Optional anchor alignment updates only map registration and is validated independently from CRS-only selection. Every offered MGA CRS has transform coverage, including known-point and round-trip coverage for GDA2020 / MGA zone 55 (EPSG:7855).
- Map failures, missing network, invalid provider response, and bad saved metadata do not crash or corrupt the drawing.
- Saving/reopening preserves all CAD entities and unknown file content; map metadata follows the agreed persistence model.
- Provider attribution and API/storage/rate-limit rules are respected; cache behavior is source-specific. Google satellite requests use a current Map Tiles API session and viewport copyright metadata. Esri requests use an ArcGIS access token and render its imagery + labels layers without persistent tile storage.
- Native and wasm checks pass; no UI dependency leaks below `ui-egui` and no network dependency leaks into geometry/rendering.
- CAD-only export remains unchanged unless a future explicit “include map” export is designed and provider terms permit it.

## 6. Main risks and decisions still open

| Decision/risk | Why it matters | Resolution gate |
|---|---|---|
| Coordinate transform crate | Accuracy, platform packaging, wasm support, and licence | Phase 0 ADR and native/wasm spike |
| Per-drawing metadata format | DXF round-trip safety and project portability | Phase 0 ADR; no entity changes |
| Street tile source | Attribution, rate limits, reliability, cache policy | Select source before production requests |
| Satellite source | Commercial/internal-use licensing, attribution, access, and cost | Separate selection and terms review; Esri World Imagery use remains conditional on current terms validation |
| Performance | Image decoding, network latency, large canvases | Async loading, bounded cache, cancellation, profiling |
| Drawing intent | CAD units alone do not establish location or CRS | Require explicit CRS selection; keep coordinate alignment opt-in and separate |

## 7. Repository references

- [CADCraft source repository](https://github.com/storytold/cadcraft)
- [`AGENTS.md`](https://github.com/storytold/cadcraft/blob/main/AGENTS.md)
- [`crates/ui-egui/src/canvas.rs`](https://github.com/storytold/cadcraft/blob/main/crates/ui-egui/src/canvas.rs)
- [`crates/ui-egui/src/lib.rs`](https://github.com/storytold/cadcraft/blob/main/crates/ui-egui/src/lib.rs)
- [`crates/engine/src/cmd/view.rs`](https://github.com/storytold/cadcraft/blob/main/crates/engine/src/cmd/view.rs)
- [`crates/render/`](https://github.com/storytold/cadcraft/tree/main/crates/render)
