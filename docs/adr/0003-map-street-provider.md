# ADR 0003: OpenStreetMap is the initial Street provider

**Status:** accepted
**Date:** 2026-10-18

## Context

CADCraft needs a street-map backdrop without binding its map model or canvas to a proprietary
provider. The initial provider must work without a bundled credential, be usable from native and
WASM builds, and preserve a path to hosted or self-hosted alternatives.

## Decision

Implement `OpenStreetMap Standard` as the initial built-in **Street** provider. It is a raster XYZ
provider with these fixed requirements:

- use `https://tile.openstreetmap.org/{z}/{x}/{y}.png` over HTTPS;
- show `© OpenStreetMap contributors` visibly whenever one of its tiles is visible;
- send an identifiable CADCraft User-Agent from native builds and preserve the browser Referer on
  web builds;
- honour `Cache-Control`, `Expires`, and `ETag` headers; if cache headers cannot be read, retain a
  tile for at least seven days;
- request only tiles currently needed by the viewport, with bounded concurrency and no background
  prefetch, offline-download command, or cache-bypass headers.

The implementation uses a provider registry and a provider-neutral `TileProvider` description:
provider ID, supported modes, URL/session construction, attribution, zoom range, request policy,
and cache policy. `OpenStreetMap Standard` is one entry, not an assumption in CRS, canvas, cache,
or command code. Future providers, including self-hosted OpenMapTiles-derived raster endpoints,
are separate entries selected by configuration.

Satellite stays unavailable in the first release. It must not alias Street or silently use a
provider that has not been approved for satellite imagery.

## Consequences

- OSM's public tile service is best-effort rather than an SLA-backed production dependency. If
  demand exceeds its policy, CADCraft can select a separately approved hosted or self-hosted
  OpenStreetMap-derived provider without changing registrations or drawing data.
- All map-off and unavailable-provider states remain usable and make no network requests.
- The visual attribution and policy tests are part of the provider adapter's test suite.

## Evidence

[OpenStreetMap's Tile Usage Policy](https://operations.osmfoundation.org/policies/tiles/) specifies
the URL, visible attribution, app identification, HTTP caching, and prohibition on bulk/prefetch
use. It also states that a suitable alternative service or self-hosting is appropriate when the
policy cannot be met.
