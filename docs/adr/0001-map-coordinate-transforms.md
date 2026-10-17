# ADR 0001: map coordinate transforms use a pure-Rust adapter

**Status:** accepted for the map Phase 0 design
**Date:** 2026-10-18

## Context

CAD drawings use arbitrary f64 world coordinates and units. A basemap cannot assume that those
coordinates are longitude/latitude. The map integration needs an explicit transformation chain:

`drawing coordinates -> project CRS -> WGS 84 geographic coordinates -> Web Mercator tile coordinates`

The desktop app and `cadcraft-web` are supported targets. A native C-library dependency is not an
acceptable foundation if it prevents a `wasm32-unknown-unknown` build. The initial acceptance case
is GDA2020 / MGA zone 55 (EPSG:7855), whose axes are easting/northing in metres.

## Decision

Add a new UI-independent `cadcraft-map` crate at the lowest layer that its dependencies permit.
It owns typed CRS identifiers, coordinate values, registration transforms, XYZ tile mathematics,
and provider-neutral metadata. It has no egui, wgpu, HTTP, filesystem, or secret dependencies.

Use `proj4rs` behind a private `TransformEngine` adapter after a pinned-version native and WASM
spike. `proj4rs` is dual licensed MIT/Apache-2.0, pure Rust, and documents a WASM target. The
adapter is mandatory: CADCraft code must not expose `proj4rs` types or accept arbitrary PROJ
strings from a drawing. Each supported CRS is an explicit allow-listed `CrsId` and vetted
definition.

The first supported set is:

- `EPSG:4326` (WGS 84 geographic), using degrees only at the public API boundary;
- `EPSG:3857` (Web Mercator) for tile coordinates; and
- `EPSG:7855` (GDA2020 / MGA zone 55), with easting/northing axis order.

Internally, angular values passed to the transform implementation are converted explicitly to and
from radians. Inputs must be finite, coordinate ranges must be validated before tile conversion,
and failures return a typed error; no transformation error may panic or silently fall back to an
assumed CRS.

## Consequences

- The Phase 1 vertical slice can use a WGS 84 registration only. EPSG:7855 ships only after the
  spike passes native and WASM tests and a known-point round trip.
- Grid-shift, height, epoch, and other high-accuracy datum transformations are out of scope for
  the first release. Their required data and accuracy must be recorded before support is added.
- The map crate remains testable without a provider or network access.

## Evidence

- [proj4rs project documentation](https://github.com/3liz/proj4rs) describes it as pure Rust,
  WASM-compatible, and not dependent on external C libraries.
- [EPSG:7855 definition](https://epsg.io/7855) specifies GDA2020 / MGA zone 55 and easting/northing
  axes. Its WGS 84 transformation is an approximately 3 m transformation, a limit that must be
  shown in future user documentation.
