//! Provider-independent foundations for CADCraft map backdrops.
//!
//! This crate deliberately has no networking, image decoding, UI, or filesystem code. Map
//! imagery is a view-only reference: these types describe registration and visible XYZ tiles,
//! never drawing entities or selection geometry.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

/// Largest zoom accepted by the generic XYZ arithmetic.
///
/// Providers declare their own (usually smaller) limit. Keeping this below 31 makes the tile
/// grid size representable in `u32` and avoids hostile shift/allocation requests.
pub const MAX_XYZ_ZOOM: u8 = 30;

/// Maximum latitude representable by the Web Mercator projection.
pub const WEB_MERCATOR_MAX_LATITUDE: f64 = 85.051_128_779_806_6;

/// Web Mercator's spherical radius, in metres.
pub const WEB_MERCATOR_RADIUS: f64 = 6_378_137.0;

/// Version of the standalone map-registration sidecar schema.
///
/// Tile imagery, provider credentials, and display state are deliberately not persistent drawing
/// data. The sidecar stores only the coordinate registration needed to redisplay an opted-in map.
pub const MAP_SIDECAR_SCHEMA_VERSION: u32 = 1;

/// Maximum number of tiles one frame may ask a source for.
pub const MAX_VISIBLE_TILES: usize = 256;

/// User-selected backdrop mode. Satellite remains unavailable until a licensed source is
/// configured, but the mode is represented so providers can be added without changing commands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MapMode {
    #[default]
    Off,
    Street,
    Satellite,
}

/// Map state associated with one open drawing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapState {
    pub mode: MapMode,
    pub opacity: f32,
    pub registration: Option<Registration>,
}

impl Default for MapState {
    fn default() -> Self {
        Self { mode: MapMode::Off, opacity: 0.65, registration: None }
    }
}

/// Stable metadata for a replaceable raster XYZ tile provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub attribution: &'static str,
    pub min_zoom: Zoom,
    pub max_zoom: Zoom,
}

/// Provider-specific tile addressing. Fetching, caching, cancellation, and decoding remain at the
/// application/UI boundary.
pub trait TileProvider {
    fn info(&self) -> ProviderInfo;
    fn tile_url(&self, tile: TileId) -> Result<String>;
}

/// Deterministic source used to prove rendering and request behavior without network access.
#[derive(Clone, Copy, Debug, Default)]
pub struct MockProvider;

impl TileProvider for MockProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "mock",
            name: "CADCraft mock map",
            attribution: "CADCraft deterministic mock tiles",
            min_zoom: Zoom(0),
            max_zoom: Zoom(MAX_XYZ_ZOOM),
        }
    }

    fn tile_url(&self, tile: TileId) -> Result<String> {
        Ok(format!("mock://{}/{}/{}", tile.zoom.value(), tile.x, tile.y))
    }
}

/// OpenStreetMap Standard provider metadata and URL addressing.
///
/// This type performs no requests. The transport must supply an identifying User-Agent on native
/// builds, honor HTTP cache headers, avoid prefetching, and show this attribution with the tiles.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenStreetMapStandard;

impl TileProvider for OpenStreetMapStandard {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "openstreetmap-standard",
            name: "OpenStreetMap Standard",
            attribution: "© OpenStreetMap contributors",
            min_zoom: Zoom(0),
            max_zoom: Zoom(19),
        }
    }

    fn tile_url(&self, tile: TileId) -> Result<String> {
        if tile.zoom.value() > self.info().max_zoom.value() {
            return Err(MapError::ProviderZoomOutOfRange);
        }
        Ok(format!("https://tile.openstreetmap.org/{}/{}/{}.png", tile.zoom.value(), tile.x, tile.y))
    }
}

/// Google Maps Platform satellite tiles. The API key and short-lived session token are supplied
/// at runtime and must never be serialized with a drawing or application profile. This type only
/// builds request URLs; callers own network, billing, attribution, and cache policy.
#[derive(Clone, Copy)]
pub struct GoogleMapsSatellite<'a> {
    api_key: &'a str,
    session: Option<&'a str>,
}

impl<'a> GoogleMapsSatellite<'a> {
    pub fn new(api_key: &'a str, session: Option<&'a str>) -> Result<Self> {
        if !valid_provider_credential(api_key) || session.is_some_and(|value| !valid_provider_credential(value)) {
            return Err(MapError::InvalidProviderCredential);
        }
        Ok(Self { api_key, session })
    }

    pub fn session_url(self) -> String {
        format!("https://tile.googleapis.com/v1/createSession?key={}", self.api_key)
    }

    /// URL for the attribution and imagery-zoom metadata for one visible viewport.
    pub fn viewport_url(self, zoom: Zoom, bounds: TileBounds, session: &str) -> Result<String> {
        if !valid_provider_credential(session) {
            return Err(MapError::InvalidProviderCredential);
        }
        if !(bounds.west.is_finite() && bounds.east.is_finite() && bounds.south.is_finite() && bounds.north.is_finite())
            || !(-180.0..=180.0).contains(&bounds.west)
            || !(-180.0..=180.0).contains(&bounds.east)
            || !(-90.0..=90.0).contains(&bounds.south)
            || !(-90.0..=90.0).contains(&bounds.north)
            || bounds.south > bounds.north
        {
            return Err(MapError::InvalidViewportBounds);
        }
        Ok(format!(
            "https://tile.googleapis.com/tile/v1/viewport?session={session}&key={}&zoom={}&north={}&south={}&east={}&west={}",
            self.api_key,
            zoom.value(),
            bounds.north,
            bounds.south,
            bounds.east,
            bounds.west
        ))
    }
}

fn valid_provider_credential(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

impl TileProvider for GoogleMapsSatellite<'_> {
    fn info(&self) -> ProviderInfo {
        ProviderInfo { id: "google-maps-satellite", name: "Google Maps Satellite", attribution: "Google Maps", min_zoom: Zoom(0), max_zoom: Zoom(22) }
    }

    fn tile_url(&self, tile: TileId) -> Result<String> {
        if tile.zoom.value() > self.info().max_zoom.value() {
            return Err(MapError::ProviderZoomOutOfRange);
        }
        let session = self.session.ok_or(MapError::ProviderSessionRequired)?;
        Ok(format!("https://tile.googleapis.com/v1/2dtiles/{}/{}/{}?session={session}&key={}", tile.zoom.value(), tile.x, tile.y, self.api_key))
    }
}

/// A supported coordinate reference system.
///
/// Axis ordering is always explicit in the associated coordinate type: geographic values are
/// longitude then latitude, projected values are easting/x then northing/y.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CrsId {
    #[serde(rename = "EPSG:4326")]
    Wgs84,
    #[serde(rename = "EPSG:3857")]
    WebMercator,
    #[serde(rename = "EPSG:7855")]
    Gda2020MgaZone55,
    #[serde(rename = "EPSG:7846")]
    Gda2020MgaZone46,
    #[serde(rename = "EPSG:7847")]
    Gda2020MgaZone47,
    #[serde(rename = "EPSG:7848")]
    Gda2020MgaZone48,
    #[serde(rename = "EPSG:7849")]
    Gda2020MgaZone49,
    #[serde(rename = "EPSG:7850")]
    Gda2020MgaZone50,
    #[serde(rename = "EPSG:7851")]
    Gda2020MgaZone51,
    #[serde(rename = "EPSG:7852")]
    Gda2020MgaZone52,
    #[serde(rename = "EPSG:7853")]
    Gda2020MgaZone53,
    #[serde(rename = "EPSG:7854")]
    Gda2020MgaZone54,
    #[serde(rename = "EPSG:7856")]
    Gda2020MgaZone56,
    #[serde(rename = "EPSG:28346")]
    Gda94MgaZone46,
    #[serde(rename = "EPSG:28347")]
    Gda94MgaZone47,
    #[serde(rename = "EPSG:28348")]
    Gda94MgaZone48,
    #[serde(rename = "EPSG:28349")]
    Gda94MgaZone49,
    #[serde(rename = "EPSG:28350")]
    Gda94MgaZone50,
    #[serde(rename = "EPSG:28351")]
    Gda94MgaZone51,
    #[serde(rename = "EPSG:28352")]
    Gda94MgaZone52,
    #[serde(rename = "EPSG:28353")]
    Gda94MgaZone53,
    #[serde(rename = "EPSG:28354")]
    Gda94MgaZone54,
    #[serde(rename = "EPSG:28355")]
    Gda94MgaZone55,
    #[serde(rename = "EPSG:28356")]
    Gda94MgaZone56,
}

impl CrsId {
    pub const MGA: [Self; 22] = [
        Self::Gda2020MgaZone46,
        Self::Gda2020MgaZone47,
        Self::Gda2020MgaZone48,
        Self::Gda2020MgaZone49,
        Self::Gda2020MgaZone50,
        Self::Gda2020MgaZone51,
        Self::Gda2020MgaZone52,
        Self::Gda2020MgaZone53,
        Self::Gda2020MgaZone54,
        Self::Gda2020MgaZone55,
        Self::Gda2020MgaZone56,
        Self::Gda94MgaZone46,
        Self::Gda94MgaZone47,
        Self::Gda94MgaZone48,
        Self::Gda94MgaZone49,
        Self::Gda94MgaZone50,
        Self::Gda94MgaZone51,
        Self::Gda94MgaZone52,
        Self::Gda94MgaZone53,
        Self::Gda94MgaZone54,
        Self::Gda94MgaZone55,
        Self::Gda94MgaZone56,
    ];

    /// Canonical EPSG identifier for persistence and user-facing status.
    pub const fn epsg(self) -> &'static str {
        match self {
            Self::Wgs84 => "EPSG:4326",
            Self::WebMercator => "EPSG:3857",
            Self::Gda2020MgaZone46 => "EPSG:7846",
            Self::Gda2020MgaZone47 => "EPSG:7847",
            Self::Gda2020MgaZone48 => "EPSG:7848",
            Self::Gda2020MgaZone49 => "EPSG:7849",
            Self::Gda2020MgaZone50 => "EPSG:7850",
            Self::Gda2020MgaZone51 => "EPSG:7851",
            Self::Gda2020MgaZone52 => "EPSG:7852",
            Self::Gda2020MgaZone53 => "EPSG:7853",
            Self::Gda2020MgaZone54 => "EPSG:7854",
            Self::Gda2020MgaZone55 => "EPSG:7855",
            Self::Gda2020MgaZone56 => "EPSG:7856",
            Self::Gda94MgaZone46 => "EPSG:28346",
            Self::Gda94MgaZone47 => "EPSG:28347",
            Self::Gda94MgaZone48 => "EPSG:28348",
            Self::Gda94MgaZone49 => "EPSG:28349",
            Self::Gda94MgaZone50 => "EPSG:28350",
            Self::Gda94MgaZone51 => "EPSG:28351",
            Self::Gda94MgaZone52 => "EPSG:28352",
            Self::Gda94MgaZone53 => "EPSG:28353",
            Self::Gda94MgaZone54 => "EPSG:28354",
            Self::Gda94MgaZone55 => "EPSG:28355",
            Self::Gda94MgaZone56 => "EPSG:28356",
        }
    }

    pub const fn mga_details(self) -> Option<(&'static str, u8)> {
        match self {
            Self::Gda2020MgaZone46 => Some(("GDA2020", 46)),
            Self::Gda2020MgaZone47 => Some(("GDA2020", 47)),
            Self::Gda2020MgaZone48 => Some(("GDA2020", 48)),
            Self::Gda2020MgaZone49 => Some(("GDA2020", 49)),
            Self::Gda2020MgaZone50 => Some(("GDA2020", 50)),
            Self::Gda2020MgaZone51 => Some(("GDA2020", 51)),
            Self::Gda2020MgaZone52 => Some(("GDA2020", 52)),
            Self::Gda2020MgaZone53 => Some(("GDA2020", 53)),
            Self::Gda2020MgaZone54 => Some(("GDA2020", 54)),
            Self::Gda2020MgaZone55 => Some(("GDA2020", 55)),
            Self::Gda2020MgaZone56 => Some(("GDA2020", 56)),
            Self::Gda94MgaZone46 => Some(("GDA94", 46)),
            Self::Gda94MgaZone47 => Some(("GDA94", 47)),
            Self::Gda94MgaZone48 => Some(("GDA94", 48)),
            Self::Gda94MgaZone49 => Some(("GDA94", 49)),
            Self::Gda94MgaZone50 => Some(("GDA94", 50)),
            Self::Gda94MgaZone51 => Some(("GDA94", 51)),
            Self::Gda94MgaZone52 => Some(("GDA94", 52)),
            Self::Gda94MgaZone53 => Some(("GDA94", 53)),
            Self::Gda94MgaZone54 => Some(("GDA94", 54)),
            Self::Gda94MgaZone55 => Some(("GDA94", 55)),
            Self::Gda94MgaZone56 => Some(("GDA94", 56)),
            _ => None,
        }
    }
}

/// A finite two-dimensional coordinate in the axis order specified by its context.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawCoordinate")]
#[serde(rename_all = "camelCase")]
pub struct Coordinate {
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCoordinate {
    x: f64,
    y: f64,
}

impl TryFrom<RawCoordinate> for Coordinate {
    type Error = MapError;

    fn try_from(value: RawCoordinate) -> Result<Self> {
        Self::new(value.x, value.y)
    }
}

impl Coordinate {
    pub fn new(x: f64, y: f64) -> Result<Self> {
        if !(x.is_finite() && y.is_finite()) {
            return Err(MapError::NonFiniteCoordinate);
        }
        Ok(Self { x, y })
    }

    pub const fn x(self) -> f64 {
        self.x
    }

    pub const fn y(self) -> f64 {
        self.y
    }
}

/// Longitude/latitude in degrees, in that order.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawLonLat")]
#[serde(rename_all = "camelCase")]
pub struct LonLat {
    longitude: f64,
    latitude: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLonLat {
    longitude: f64,
    latitude: f64,
}

impl TryFrom<RawLonLat> for LonLat {
    type Error = MapError;

    fn try_from(value: RawLonLat) -> Result<Self> {
        Self::new(value.longitude, value.latitude)
    }
}

impl LonLat {
    pub fn new(longitude: f64, latitude: f64) -> Result<Self> {
        if !(longitude.is_finite() && latitude.is_finite()) {
            return Err(MapError::NonFiniteCoordinate);
        }
        if !(-180.0..=180.0).contains(&longitude) {
            return Err(MapError::LongitudeOutOfRange);
        }
        if !(-WEB_MERCATOR_MAX_LATITUDE..=WEB_MERCATOR_MAX_LATITUDE).contains(&latitude) {
            return Err(MapError::LatitudeOutOfRange);
        }
        Ok(Self { longitude, latitude })
    }

    pub const fn longitude(self) -> f64 {
        self.longitude
    }

    pub const fn latitude(self) -> f64 {
        self.latitude
    }
}

/// An affine registration from drawing units to an explicitly selected project CRS.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawRegistration")]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    crs: CrsId,
    drawing_anchor: Coordinate,
    project_anchor: Coordinate,
    /// Project units per drawing unit; must be positive and finite.
    scale: f64,
    /// Counter-clockwise angle in radians from drawing X to project X.
    rotation: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRegistration {
    crs: CrsId,
    drawing_anchor: Coordinate,
    project_anchor: Coordinate,
    scale: f64,
    rotation: f64,
}

impl TryFrom<RawRegistration> for Registration {
    type Error = MapError;

    fn try_from(value: RawRegistration) -> Result<Self> {
        Self::new(value.crs, value.drawing_anchor, value.project_anchor, value.scale, value.rotation)
    }
}

impl Registration {
    pub fn new(crs: CrsId, drawing_anchor: Coordinate, project_anchor: Coordinate, scale: f64, rotation: f64) -> Result<Self> {
        if !(scale.is_finite() && scale > 0.0) {
            return Err(MapError::InvalidScale);
        }
        if !rotation.is_finite() {
            return Err(MapError::InvalidRotation);
        }
        Ok(Self { crs, drawing_anchor, project_anchor, scale, rotation })
    }

    pub const fn crs(self) -> CrsId {
        self.crs
    }

    pub const fn drawing_anchor(self) -> Coordinate {
        self.drawing_anchor
    }

    pub const fn project_anchor(self) -> Coordinate {
        self.project_anchor
    }

    pub const fn scale(self) -> f64 {
        self.scale
    }

    pub const fn rotation(self) -> f64 {
        self.rotation
    }

    /// Convert a finite drawing point into this registration's project coordinate.
    pub fn to_project(self, drawing: Coordinate) -> Result<Coordinate> {
        let dx = drawing.x - self.drawing_anchor.x;
        let dy = drawing.y - self.drawing_anchor.y;
        let (sin, cos) = self.rotation.sin_cos();
        Coordinate::new(self.project_anchor.x + self.scale * (dx * cos - dy * sin), self.project_anchor.y + self.scale * (dx * sin + dy * cos))
    }

    /// Convert a finite project point into drawing coordinates.
    pub fn to_drawing(self, project: Coordinate) -> Result<Coordinate> {
        let dx = (project.x - self.project_anchor.x) / self.scale;
        let dy = (project.y - self.project_anchor.y) / self.scale;
        let (sin, cos) = self.rotation.sin_cos();
        Coordinate::new(self.drawing_anchor.x + dx * cos + dy * sin, self.drawing_anchor.y - dx * sin + dy * cos)
    }
}

/// The versioned, provider-independent data persisted beside a drawing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MapMetadata {
    schema_version: u32,
    registration: Registration,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMapMetadata {
    schema_version: u32,
    registration: Registration,
}

impl TryFrom<RawMapMetadata> for MapMetadata {
    type Error = MapError;

    fn try_from(value: RawMapMetadata) -> Result<Self> {
        if value.schema_version != MAP_SIDECAR_SCHEMA_VERSION {
            return Err(MapError::UnsupportedSidecarVersion);
        }
        Ok(Self { schema_version: value.schema_version, registration: value.registration })
    }
}

impl<'de> Deserialize<'de> for MapMetadata {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RawMapMetadata::deserialize(deserializer)?.try_into().map_err(serde::de::Error::custom)
    }
}

impl MapMetadata {
    pub const fn new(registration: Registration) -> Self {
        Self { schema_version: MAP_SIDECAR_SCHEMA_VERSION, registration }
    }

    pub const fn schema_version(self) -> u32 {
        self.schema_version
    }

    pub const fn registration(self) -> Registration {
        self.registration
    }
}

/// A validated XYZ zoom level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8")]
pub struct Zoom(u8);

impl Zoom {
    pub fn new(value: u8) -> Result<Self> {
        if value > MAX_XYZ_ZOOM {
            return Err(MapError::ZoomOutOfRange);
        }
        Ok(Self(value))
    }

    pub const fn value(self) -> u8 {
        self.0
    }

    pub const fn tile_count(self) -> u32 {
        1_u32 << self.0
    }
}

impl TryFrom<u8> for Zoom {
    type Error = MapError;

    fn try_from(value: u8) -> Result<Self> {
        Self::new(value)
    }
}

/// A single tile in the standard XYZ scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawTileId")]
#[serde(rename_all = "camelCase")]
pub struct TileId {
    zoom: Zoom,
    x: u32,
    y: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTileId {
    zoom: Zoom,
    x: u32,
    y: u32,
}

impl TryFrom<RawTileId> for TileId {
    type Error = MapError;

    fn try_from(value: RawTileId) -> Result<Self> {
        Self::new(value.zoom, value.x, value.y)
    }
}

impl TileId {
    pub fn new(zoom: Zoom, x: u32, y: u32) -> Result<Self> {
        let n = zoom.tile_count();
        if x >= n || y >= n {
            return Err(MapError::TileOutOfRange);
        }
        Ok(Self { zoom, x, y })
    }

    pub const fn zoom(self) -> Zoom {
        self.zoom
    }

    pub const fn x(self) -> u32 {
        self.x
    }

    pub const fn y(self) -> u32 {
        self.y
    }
}

/// Geographic extent of an XYZ tile, in WGS 84 degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileBounds {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

/// Tile containing a geographic point. At the anti-meridian, 180° belongs to the easternmost tile.
pub fn tile_at(point: LonLat, zoom: Zoom) -> TileId {
    let n = f64::from(zoom.tile_count());
    let x = ((point.longitude + 180.0) / 360.0 * n).floor().clamp(0.0, n - 1.0) as u32;
    let latitude_radians = point.latitude.to_radians();
    let y = ((1.0 - (latitude_radians.tan() + latitude_radians.cos().recip()).ln() / PI) / 2.0 * n).floor().clamp(0.0, n - 1.0) as u32;
    // Inputs are clamped to representable ranges above, so these values meet TileId's invariant.
    TileId { zoom, x, y }
}

/// WGS 84 bounds of a validated XYZ tile.
pub fn tile_bounds(tile: TileId) -> TileBounds {
    let n = f64::from(tile.zoom.tile_count());
    let longitude = |x: u32| f64::from(x) / n * 360.0 - 180.0;
    let latitude = |y: u32| {
        let a = PI * (1.0 - 2.0 * f64::from(y) / n);
        a.sinh().atan().to_degrees()
    };
    TileBounds { west: longitude(tile.x), south: latitude(tile.y + 1), east: longitude(tile.x + 1), north: latitude(tile.y) }
}

/// Transform a supported project coordinate into WGS 84 longitude/latitude.
///
/// Geographic values are returned in degrees, even though the PROJ-compatible adapter uses
/// radians internally. GDA94/GDA2020-to-WGS84 is approximated for the initial map backdrop; this
/// is not a survey-grade epoch/grid-shift transformation.
pub fn to_wgs84(crs: CrsId, point: Coordinate) -> Result<LonLat> {
    match crs {
        CrsId::Wgs84 => LonLat::new(point.x, point.y),
        CrsId::WebMercator => web_mercator_to_wgs84(point),
        _ if crs.mga_details().is_some() => proj_transform(&mga_proj(crs)?, WGS84, point),
        _ => Err(MapError::TransformationFailed),
    }
}

/// Transform WGS 84 longitude/latitude into a supported project CRS.
pub fn from_wgs84(crs: CrsId, point: LonLat) -> Result<Coordinate> {
    match crs {
        CrsId::Wgs84 => Coordinate::new(point.longitude, point.latitude),
        CrsId::WebMercator => wgs84_to_web_mercator(point),
        _ if crs.mga_details().is_some() => {
            let radians = Coordinate::new(point.longitude.to_radians(), point.latitude.to_radians())?;
            proj_transform_raw(WGS84, &mga_proj(crs)?, radians)
        }
        _ => Err(MapError::TransformationFailed),
    }
}

const WGS84: &str = "+proj=longlat +datum=WGS84 +no_defs +type=crs";

fn mga_proj(crs: CrsId) -> Result<String> {
    let Some((_, zone)) = crs.mga_details() else { return Err(MapError::TransformationFailed) };
    // The initial basemap transform treats GDA94 and GDA2020 as GRS80 realizations with a
    // zero-parameter datum operation. This is suitable for visual map context, not survey work.
    Ok(format!("+proj=utm +zone={zone} +south +ellps=GRS80 +towgs84=0,0,0,0,0,0,0 +units=m +no_defs +type=crs"))
}

fn proj_transform(from: &str, to: &str, point: Coordinate) -> Result<LonLat> {
    let radians = proj_transform_raw(from, to, point)?;
    LonLat::new(radians.x.to_degrees(), radians.y.to_degrees())
}

fn proj_transform_raw(from: &str, to: &str, point: Coordinate) -> Result<Coordinate> {
    let from = proj4rs::proj::Proj::from_proj_string(from).map_err(|_| MapError::TransformationFailed)?;
    let to = proj4rs::proj::Proj::from_proj_string(to).map_err(|_| MapError::TransformationFailed)?;
    let mut value = (point.x, point.y, 0.0);
    proj4rs::transform::transform(&from, &to, &mut value).map_err(|_| MapError::TransformationFailed)?;
    Coordinate::new(value.0, value.1).map_err(|_| MapError::TransformationFailed)
}

fn web_mercator_to_wgs84(point: Coordinate) -> Result<LonLat> {
    let limit = PI * WEB_MERCATOR_RADIUS;
    if point.x.abs() > limit || point.y.abs() > limit {
        return Err(MapError::WebMercatorOutOfRange);
    }
    let longitude = (point.x / WEB_MERCATOR_RADIUS).to_degrees();
    let latitude = (2.0 * (point.y / WEB_MERCATOR_RADIUS).exp().atan() - PI / 2.0).to_degrees();
    LonLat::new(longitude, latitude).map_err(|_| MapError::TransformationFailed)
}

fn wgs84_to_web_mercator(point: LonLat) -> Result<Coordinate> {
    let longitude = WEB_MERCATOR_RADIUS * point.longitude.to_radians();
    let latitude = WEB_MERCATOR_RADIUS * (PI / 4.0 + point.latitude.to_radians() / 2.0).tan().ln();
    Coordinate::new(longitude, latitude).map_err(|_| MapError::TransformationFailed)
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    #[error("map coordinate must be finite")]
    NonFiniteCoordinate,
    #[error("longitude must be within -180 to 180 degrees")]
    LongitudeOutOfRange,
    #[error("latitude is outside the Web Mercator range")]
    LatitudeOutOfRange,
    #[error("registration scale must be finite and greater than zero")]
    InvalidScale,
    #[error("registration rotation must be finite")]
    InvalidRotation,
    #[error("XYZ zoom is outside the supported range")]
    ZoomOutOfRange,
    #[error("tile coordinate is outside its zoom grid")]
    TileOutOfRange,
    #[error("Web Mercator coordinate is outside its supported world extent")]
    WebMercatorOutOfRange,
    #[error("coordinate transformation failed")]
    TransformationFailed,
    #[error("map sidecar schema version is unsupported")]
    UnsupportedSidecarVersion,
    #[error("tile zoom is outside the provider's supported range")]
    ProviderZoomOutOfRange,
    #[error("provider credential is invalid")]
    InvalidProviderCredential,
    #[error("provider session is required")]
    ProviderSessionRequired,
    #[error("viewport bounds are invalid")]
    InvalidViewportBounds,
}

pub type Result<T> = std::result::Result<T, MapError>;

#[cfg(test)]
#[allow(clippy::unreachable)]
mod tests {
    use super::*;

    fn coordinate(x: f64, y: f64) -> Coordinate {
        match Coordinate::new(x, y) {
            Ok(value) => value,
            Err(error) => unreachable!("test coordinate must be valid: {error}"),
        }
    }

    fn zoom(value: u8) -> Zoom {
        match Zoom::new(value) {
            Ok(value) => value,
            Err(error) => unreachable!("test zoom must be valid: {error}"),
        }
    }

    fn lon_lat(longitude: f64, latitude: f64) -> LonLat {
        match LonLat::new(longitude, latitude) {
            Ok(value) => value,
            Err(error) => unreachable!("test longitude/latitude must be valid: {error}"),
        }
    }

    #[test]
    fn registration_round_trips() {
        let registration = match Registration::new(CrsId::Gda2020MgaZone55, coordinate(10.0, -5.0), coordinate(320_000.0, 5_813_000.0), 2.0, PI / 3.0)
        {
            Ok(value) => value,
            Err(error) => unreachable!("test registration must be valid: {error}"),
        };
        let drawing = coordinate(24.0, 8.0);
        let project = match registration.to_project(drawing) {
            Ok(value) => value,
            Err(error) => unreachable!("conversion must be finite: {error}"),
        };
        let back = match registration.to_drawing(project) {
            Ok(value) => value,
            Err(error) => unreachable!("conversion must be finite: {error}"),
        };
        assert!((back.x() - drawing.x()).abs() < 1e-8);
        assert!((back.y() - drawing.y()).abs() < 1e-8);
    }

    #[test]
    fn xyz_covers_world_edges_without_overflow() {
        let zoom = zoom(MAX_XYZ_ZOOM);
        let east = tile_at(lon_lat(180.0, WEB_MERCATOR_MAX_LATITUDE), zoom);
        let west = tile_at(lon_lat(-180.0, -WEB_MERCATOR_MAX_LATITUDE), zoom);
        assert_eq!(east.x(), zoom.tile_count() - 1);
        assert_eq!(east.y(), 0);
        assert_eq!(west.x(), 0);
        assert_eq!(west.y(), zoom.tile_count() - 1);
    }

    #[test]
    fn tile_bounds_contain_the_origin() {
        let origin = tile_at(lon_lat(0.0, 0.0), zoom(3));
        let bounds = tile_bounds(origin);
        assert!(bounds.west <= 0.0 && bounds.east >= 0.0);
        assert!(bounds.south <= 0.0 && bounds.north >= 0.0);
    }

    #[test]
    fn hostile_parameters_return_errors_without_panicking() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(Coordinate::new(bad, 0.0), Err(MapError::NonFiniteCoordinate));
            assert_eq!(LonLat::new(bad, 0.0), Err(MapError::NonFiniteCoordinate));
            assert_eq!(Registration::new(CrsId::Wgs84, coordinate(0.0, 0.0), coordinate(0.0, 0.0), bad, 0.0), Err(MapError::InvalidScale));
            assert_eq!(Registration::new(CrsId::Wgs84, coordinate(0.0, 0.0), coordinate(0.0, 0.0), 1.0, bad), Err(MapError::InvalidRotation));
        }
        assert_eq!(LonLat::new(180.1, 0.0), Err(MapError::LongitudeOutOfRange));
        assert_eq!(LonLat::new(0.0, WEB_MERCATOR_MAX_LATITUDE + 0.1), Err(MapError::LatitudeOutOfRange));
        assert_eq!(Registration::new(CrsId::Wgs84, coordinate(0.0, 0.0), coordinate(0.0, 0.0), 0.0, 0.0), Err(MapError::InvalidScale));
        assert_eq!(Zoom::new(MAX_XYZ_ZOOM + 1), Err(MapError::ZoomOutOfRange));
        assert_eq!(TileId::new(zoom(2), 4, 0), Err(MapError::TileOutOfRange));
    }

    #[test]
    fn deserialization_keeps_type_invariants() {
        assert!(serde_json::from_str::<Coordinate>(r#"{"x":1.0,"y":null}"#).is_err());
        assert!(serde_json::from_str::<LonLat>(r#"{"longitude":181.0,"latitude":0.0}"#).is_err());
        assert!(
            serde_json::from_str::<Registration>(
                r#"{"crs":"EPSG:4326","drawingAnchor":{"x":0.0,"y":0.0},"projectAnchor":{"x":0.0,"y":0.0},"scale":0.0,"rotation":0.0}"#,
            )
            .is_err()
        );
        assert!(serde_json::from_str::<Zoom>("31").is_err());
        assert!(serde_json::from_str::<TileId>(r#"{"zoom":2,"x":4,"y":0}"#).is_err());
        assert!(serde_json::from_str::<MapMetadata>(r#"{"schemaVersion":2,"registration":{"crs":"EPSG:4326","drawingAnchor":{"x":0.0,"y":0.0},"projectAnchor":{"x":0.0,"y":0.0},"scale":1.0,"rotation":0.0}}"#).is_err());
    }

    #[test]
    fn web_mercator_round_trips() {
        let source = lon_lat(144.9631, -37.8136);
        let projected = match from_wgs84(CrsId::WebMercator, source) {
            Ok(value) => value,
            Err(error) => unreachable!("Web Mercator projection must succeed: {error}"),
        };
        let back = match to_wgs84(CrsId::WebMercator, projected) {
            Ok(value) => value,
            Err(error) => unreachable!("Web Mercator inverse must succeed: {error}"),
        };
        assert!((back.longitude() - source.longitude()).abs() < 1e-10);
        assert!((back.latitude() - source.latitude()).abs() < 1e-10);
    }

    #[test]
    fn epsg_7855_uses_mga_zone_55_false_origin() {
        let project = match from_wgs84(CrsId::Gda2020MgaZone55, lon_lat(147.0, 0.0)) {
            Ok(value) => value,
            Err(error) => unreachable!("MGA central meridian must project: {error}"),
        };
        assert!((project.x() - 500_000.0).abs() < 1e-6);
        assert!((project.y() - 10_000_000.0).abs() < 1e-6);
        let back = match to_wgs84(CrsId::Gda2020MgaZone55, project) {
            Ok(value) => value,
            Err(error) => unreachable!("MGA inverse must succeed: {error}"),
        };
        assert!((back.longitude() - 147.0).abs() < 1e-9);
        assert!(back.latitude().abs() < 1e-9);
    }

    #[test]
    fn every_offered_mga_zone_round_trips_at_its_central_meridian() {
        for crs in CrsId::MGA {
            let Some((_, zone)) = crs.mga_details() else { unreachable!("catalogue entries are MGA CRSs") };
            let longitude = f64::from(zone) * 6.0 - 183.0;
            let project = match from_wgs84(crs, lon_lat(longitude, -33.0)) {
                Ok(value) => value,
                Err(error) => unreachable!("MGA forward transform should work for {crs:?}: {error}"),
            };
            assert!((project.x() - 500_000.0).abs() < 1e-5, "{} easting", crs.epsg());
            let back = match to_wgs84(crs, project) {
                Ok(value) => value,
                Err(error) => unreachable!("MGA inverse transform should work for {crs:?}: {error}"),
            };
            assert!((back.longitude() - longitude).abs() < 1e-8, "{} longitude", crs.epsg());
            assert!((back.latitude() + 33.0).abs() < 1e-8, "{} latitude", crs.epsg());
        }
    }

    #[test]
    fn providers_are_replaceable_and_validate_zoom() {
        let tile = match TileId::new(zoom(3), 4, 2) {
            Ok(value) => value,
            Err(error) => unreachable!("test tile must be valid: {error}"),
        };
        assert_eq!(MockProvider.tile_url(tile).as_deref(), Ok("mock://3/4/2"));
        assert_eq!(OpenStreetMapStandard.tile_url(tile).as_deref(), Ok("https://tile.openstreetmap.org/3/4/2.png"));
        let high = match TileId::new(zoom(20), 0, 0) {
            Ok(value) => value,
            Err(error) => unreachable!("test tile must be valid: {error}"),
        };
        assert_eq!(OpenStreetMapStandard.tile_url(high), Err(MapError::ProviderZoomOutOfRange));
    }

    #[test]
    fn google_satellite_provider_builds_safe_session_and_tile_urls() {
        let tile = match TileId::new(zoom(15), 6294, 13288) {
            Ok(value) => value,
            Err(error) => unreachable!("test tile must be valid: {error}"),
        };
        let provider = match GoogleMapsSatellite::new("AIza_test-123", None) {
            Ok(value) => value,
            Err(error) => unreachable!("test key must be URL-safe: {error}"),
        };
        assert_eq!(provider.tile_url(tile), Err(MapError::ProviderSessionRequired));
        assert_eq!(provider.session_url(), "https://tile.googleapis.com/v1/createSession?key=AIza_test-123");
        let provider = match GoogleMapsSatellite::new("AIza_test-123", Some("session-123")) {
            Ok(value) => value,
            Err(error) => unreachable!("test session must be URL-safe: {error}"),
        };
        assert_eq!(
            provider.tile_url(tile).as_deref(),
            Ok("https://tile.googleapis.com/v1/2dtiles/15/6294/13288?session=session-123&key=AIza_test-123")
        );
        assert!(GoogleMapsSatellite::new("key&other=value", None).is_err());
    }

    #[test]
    fn google_viewport_url_validates_bounds() {
        let provider = match GoogleMapsSatellite::new("AIza_test", Some("session")) {
            Ok(value) => value,
            Err(error) => unreachable!("test key and session must be URL-safe: {error}"),
        };
        let bounds = TileBounds { west: 144.0, south: -38.0, east: 145.0, north: -37.0 };
        assert_eq!(
            provider.viewport_url(zoom(12), bounds, "session").as_deref(),
            Ok("https://tile.googleapis.com/tile/v1/viewport?session=session&key=AIza_test&zoom=12&north=-37&south=-38&east=145&west=144")
        );
        assert_eq!(provider.viewport_url(zoom(12), TileBounds { north: 100.0, ..bounds }, "session"), Err(MapError::InvalidViewportBounds));
    }
}
