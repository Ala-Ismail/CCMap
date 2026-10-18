//! Bounded OpenStreetMap tile transport and decoded texture cache.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
#[cfg(not(target_arch = "wasm32"))]
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use cadcraft_map::{EsriWorldImagery, GoogleMapsSatellite, OpenStreetMapStandard, TileBounds, TileId, TileProvider, Zoom};
use egui::{ColorImage, Context, TextureHandle, TextureOptions};

const MAX_REQUESTS: usize = 4;
const MAX_TEXTURES: usize = 96;
const MAX_TILE_BYTES: usize = 1024 * 1024;
const RETRY_DELAY: Duration = Duration::from_secs(60);
// Google Map Tiles API hybrid imagery is a satellite base map with the roadmap layer
// composited by Google into each returned tile. This keeps one aligned, attributed tile set.
const GOOGLE_HYBRID_SESSION_BODY: &[u8] = br#"{"mapType":"satellite","layerTypes":["layerRoadmap"],"overlay":false,"language":"en","region":"AU"}"#;

type Completed = (TileId, ehttp::Result<ehttp::Response>);

struct Entry {
    texture: TextureHandle,
    used: u64,
    expires_at: Option<Instant>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(serde::Serialize, serde::Deserialize)]
struct DiskMeta {
    expires: u64,
    etag: Option<String>,
    last_modified: Option<String>,
}

pub struct TileCache {
    entries: HashMap<TileId, Entry>,
    queued: VecDeque<TileId>,
    pending: HashSet<TileId>,
    failed: HashMap<TileId, Instant>,
    tx: Sender<Completed>,
    rx: Receiver<Completed>,
    frame: u64,
    #[cfg(not(target_arch = "wasm32"))]
    disk_root: Option<PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    disk_checked: HashSet<TileId>,
}

impl Default for TileCache {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            entries: HashMap::new(),
            queued: VecDeque::new(),
            pending: HashSet::new(),
            failed: HashMap::new(),
            tx,
            rx,
            frame: 0,
            #[cfg(not(target_arch = "wasm32"))]
            disk_root: cache_root(),
            #[cfg(not(target_arch = "wasm32"))]
            disk_checked: HashSet::new(),
        }
    }
}

impl TileCache {
    pub fn begin_frame(&mut self, ctx: &Context) {
        self.frame = self.frame.wrapping_add(1);
        while let Ok((id, result)) = self.rx.try_recv() {
            self.pending.remove(&id);
            match result {
                Ok(response) if (response.ok || response.status == 304) && response.bytes.len() <= MAX_TILE_BYTES => {
                    #[cfg(not(target_arch = "wasm32"))]
                    let bytes = if response.status == 304 {
                        self.refresh_disk(id, &response);
                        self.read_disk(id).map(|(bytes, _)| bytes)
                    } else {
                        self.write_disk(id, &response);
                        Some(response.bytes)
                    };
                    #[cfg(target_arch = "wasm32")]
                    let bytes = Some(response.bytes);
                    if let Some(texture) = bytes.as_deref().and_then(|bytes| decode_texture(ctx, id, bytes)) {
                        self.entries.insert(id, Entry { texture, used: self.frame, expires_at: None });
                        self.failed.remove(&id);
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            self.disk_checked.remove(&id);
                        }
                    } else {
                        self.failed.insert(id, Instant::now());
                    }
                }
                _ => {
                    self.failed.insert(id, Instant::now());
                }
            }
        }
        if self.entries.len() > MAX_TEXTURES {
            let mut by_age: Vec<_> = self.entries.iter().map(|(id, entry)| (*id, entry.used)).collect();
            by_age.sort_unstable_by_key(|(_, used)| *used);
            for (id, _) in by_age.into_iter().take(self.entries.len() - MAX_TEXTURES) {
                self.entries.remove(&id);
            }
        }
        self.failed.retain(|_, at| at.elapsed() < RETRY_DELAY);
        #[cfg(not(target_arch = "wasm32"))]
        if self.disk_checked.len() > 2048 {
            self.disk_checked.clear();
        }
    }

    pub fn texture(&mut self, ctx: &Context, id: TileId) -> Option<&TextureHandle> {
        #[cfg(target_arch = "wasm32")]
        let _ = ctx;
        #[cfg(not(target_arch = "wasm32"))]
        if !self.entries.contains_key(&id)
            && self.disk_checked.insert(id)
            && let Some((bytes, meta)) = self.read_disk(id)
            && meta.expires > unix_now()
            && let Some(texture) = decode_texture(ctx, id, &bytes)
        {
            self.entries.insert(id, Entry { texture, used: self.frame, expires_at: None });
        }
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.used = self.frame;
            return Some(&entry.texture);
        }
        if !self.pending.contains(&id) && !self.queued.contains(&id) && !self.failed.contains_key(&id) && self.queued.len() < 256 {
            self.queued.push_back(id);
        }
        None
    }

    pub fn request_visible(&mut self, ctx: &Context, visible: &HashSet<TileId>) {
        self.queued.retain(|id| visible.contains(id));
        while self.pending.len() < MAX_REQUESTS {
            let Some(id) = self.queued.pop_front() else { break };
            if !visible.contains(&id) || self.entries.contains_key(&id) || !self.pending.insert(id) {
                continue;
            }
            let Ok(url) = OpenStreetMapStandard.tile_url(id) else {
                self.pending.remove(&id);
                continue;
            };
            let request = ehttp::Request::get(url).with_timeout(Some(Duration::from_secs(20)));
            #[cfg(not(target_arch = "wasm32"))]
            let request = {
                let mut request =
                    request.with_header("User-Agent", concat!("CADCraft/", env!("CARGO_PKG_VERSION"), " (+https://github.com/storytold/cadcraft)"));
                if let Some((_, meta)) = self.read_disk(id) {
                    if let Some(etag) = meta.etag {
                        request = request.with_header("If-None-Match", etag);
                    } else if let Some(last_modified) = meta.last_modified {
                        request = request.with_header("If-Modified-Since", last_modified);
                    }
                }
                request
            };
            let tx = self.tx.clone();
            let ctx = ctx.clone();
            ehttp::fetch(request, move |result| {
                let _ = tx.send((id, result));
                ctx.request_repaint();
            });
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn disk_path(&self, id: TileId) -> Option<PathBuf> {
        Some(self.disk_root.as_ref()?.join(id.zoom().value().to_string()).join(id.x().to_string()).join(format!("{}.png", id.y())))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn read_disk(&self, id: TileId) -> Option<(Vec<u8>, DiskMeta)> {
        let path = self.disk_path(id)?;
        let meta: DiskMeta = serde_json::from_slice(&fs::read(path.with_extension("json")).ok()?).ok()?;
        let length = fs::metadata(&path).ok()?.len();
        if length > MAX_TILE_BYTES as u64 {
            return None;
        }
        let bytes = fs::read(path).ok()?;
        if bytes.len() > MAX_TILE_BYTES {
            return None;
        }
        Some((bytes, meta))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn write_disk(&self, id: TileId, response: &ehttp::Response) {
        let Some(path) = self.disk_path(id) else { return };
        let control = response.headers.get("cache-control").unwrap_or("");
        if control.split(',').any(|part| part.trim().eq_ignore_ascii_case("no-store")) {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(path.with_extension("json"));
            return;
        }
        let max_age = control.split(',').find_map(|part| part.trim().strip_prefix("max-age=").and_then(|age| age.parse::<u64>().ok()));
        let revalidate = control.split(',').any(|part| part.trim().eq_ignore_ascii_case("no-cache"));
        let meta = DiskMeta {
            expires: unix_now().saturating_add(if revalidate { 0 } else { max_age.unwrap_or(7 * 24 * 60 * 60) }),
            etag: response.headers.get("etag").filter(|value| value.len() <= 256).map(str::to_owned),
            last_modified: response.headers.get("last-modified").filter(|value| value.len() <= 256).map(str::to_owned),
        };
        let Some(parent) = path.parent() else { return };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        if fs::write(&path, &response.bytes).is_ok()
            && let Ok(data) = serde_json::to_vec(&meta)
        {
            let _ = fs::write(path.with_extension("json"), data);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn refresh_disk(&self, id: TileId, response: &ehttp::Response) {
        let Some(path) = self.disk_path(id) else { return };
        let Ok(data) = fs::read(path.with_extension("json")) else { return };
        let Ok(mut meta) = serde_json::from_slice::<DiskMeta>(&data) else { return };
        let control = response.headers.get("cache-control").unwrap_or("");
        let max_age = control.split(',').find_map(|part| part.trim().strip_prefix("max-age=").and_then(|age| age.parse::<u64>().ok()));
        meta.expires = unix_now().saturating_add(max_age.unwrap_or(7 * 24 * 60 * 60));
        if let Ok(data) = serde_json::to_vec(&meta) {
            let _ = fs::write(path.with_extension("json"), data);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn cache_root() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(std::env::var_os("HOME")?).join("Library/Caches");
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let base =
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    if !base.is_absolute() {
        return None;
    }
    Some(base.join("cadcraft").join("map-tiles").join("openstreetmap-standard"))
}

#[cfg(not(target_arch = "wasm32"))]
fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_secs())
}

fn decode_texture(ctx: &Context, id: TileId, bytes: &[u8]) -> Option<TextureHandle> {
    let reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let (width, height) = reader.into_dimensions().ok()?;
    if width != 256 || height != 256 {
        return None;
    }
    let rgba = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?.to_rgba8();
    let pixels = ColorImage::from_rgba_unmultiplied([256, 256], rgba.as_raw());
    Some(ctx.load_texture(format!("osm-{}-{}-{}", id.zoom().value(), id.x(), id.y()), pixels, TextureOptions::LINEAR))
}

// Google Maps content is intentionally memory-only: Google permits only limited storage and
// requires current viewport attribution. This cache requests only tiles in the active viewport.
const GOOGLE_SESSION_LIFETIME: Duration = Duration::from_secs(12 * 24 * 60 * 60);
const GOOGLE_RETRY_DELAY: Duration = Duration::from_secs(30);
#[cfg(all(not(target_arch = "wasm32"), any(target_os = "windows", target_os = "macos", target_os = "linux")))]
const GOOGLE_KEYRING_SERVICE: &str = "CADCraft";
const GOOGLE_KEYRING_USER: &str = "Google Maps Platform API key";
const ESRI_KEYRING_USER: &str = "ArcGIS Location Platform API key";

enum GoogleCompleted {
    Session(u64, ehttp::Result<ehttp::Response>),
    Viewport(u64, ehttp::Result<ehttp::Response>),
    Tile(u64, TileId, ehttp::Result<ehttp::Response>),
}

pub struct GoogleTileCache {
    api_key: Option<String>,
    generation: u64,
    session: Option<(String, Instant)>,
    session_pending: bool,
    session_retry: Option<Instant>,
    viewport_key: Option<String>,
    viewport_pending: bool,
    viewport_retry: Option<Instant>,
    attribution: Option<String>,
    // Deliberately contains only a request stage and HTTP status; never a URL, key,
    // session token, or response body.
    error: Option<String>,
    entries: HashMap<TileId, Entry>,
    queued: VecDeque<TileId>,
    pending: HashSet<TileId>,
    failed: HashMap<TileId, Instant>,
    visible_count: usize,
    tile_requests_this_session: u64,
    tiles_loaded_this_session: u64,
    key_remembered: bool,
    tx: Sender<GoogleCompleted>,
    rx: Receiver<GoogleCompleted>,
    frame: u64,
}

impl Default for GoogleTileCache {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            api_key: None,
            generation: 0,
            session: None,
            session_pending: false,
            session_retry: None,
            viewport_key: None,
            viewport_pending: false,
            viewport_retry: None,
            attribution: None,
            error: None,
            entries: HashMap::new(),
            queued: VecDeque::new(),
            pending: HashSet::new(),
            failed: HashMap::new(),
            visible_count: 0,
            tile_requests_this_session: 0,
            tiles_loaded_this_session: 0,
            key_remembered: false,
            tx,
            rx,
            frame: 0,
        }
    }
}

impl GoogleTileCache {
    /// Keep the BYOK key only in this process. It is not part of UI preferences or drawing data.
    pub fn set_api_key(&mut self, key: Option<String>) -> Result<(), String> {
        let key = key.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
        if key.as_deref() == self.api_key.as_deref() {
            return Ok(());
        }
        if let Some(value) = key.as_deref() {
            GoogleMapsSatellite::new(value, None).map_err(|_| "Google Maps API key contains unsupported characters".to_owned())?;
        }
        self.generation = self.generation.wrapping_add(1);
        self.api_key = key;
        self.session = None;
        self.session_pending = false;
        self.session_retry = None;
        self.viewport_key = None;
        self.viewport_pending = false;
        self.viewport_retry = None;
        self.attribution = None;
        self.error = None;
        self.entries.clear();
        self.queued.clear();
        self.pending.clear();
        self.failed.clear();
        self.visible_count = 0;
        self.tile_requests_this_session = 0;
        self.tiles_loaded_this_session = 0;
        Ok(())
    }

    /// Load a previously opted-in key from the current OS user's credential store.
    pub fn load_remembered_key(&mut self) -> Result<bool, &'static str> {
        let Some(key) = read_saved_key(GOOGLE_KEYRING_USER)? else {
            self.key_remembered = false;
            return Ok(false);
        };
        self.set_api_key(Some(key)).map_err(|_| "The saved Google Maps key is invalid")?;
        self.key_remembered = true;
        Ok(true)
    }

    pub fn can_remember_key() -> bool {
        cfg!(all(not(target_arch = "wasm32"), any(target_os = "windows", target_os = "macos", target_os = "linux")))
    }

    pub fn key_is_remembered(&self) -> bool {
        self.key_remembered
    }

    /// Persist the current runtime key in the per-user OS credential store.
    pub fn remember_current_key(&mut self) -> Result<(), &'static str> {
        let key = self.api_key.as_deref().ok_or("Enter a Google Maps API key first")?;
        write_saved_key(GOOGLE_KEYRING_USER, key)?;
        self.key_remembered = true;
        Ok(())
    }

    /// Remove the per-user OS credential without affecting the active in-memory key.
    pub fn forget_remembered_key(&mut self) -> Result<(), &'static str> {
        delete_saved_key(GOOGLE_KEYRING_USER)?;
        self.key_remembered = false;
        Ok(())
    }

    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    pub fn attribution(&self) -> Option<&str> {
        self.attribution.as_deref()
    }

    pub fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// A safe, user-facing indication of the Google request currently being made.
    pub fn request_status(&self) -> &'static str {
        if self.session_pending {
            "Google Maps · requesting satellite session"
        } else if self.viewport_pending {
            "Google Maps · requesting viewport attribution"
        } else if !self.pending.is_empty() {
            "Google Maps · downloading satellite tiles"
        } else if self.session.is_none() {
            "Google Maps · waiting to request satellite session"
        } else if self.attribution.is_none() {
            "Google Maps · waiting for viewport attribution"
        } else {
            "Google Maps · waiting for satellite tiles"
        }
    }

    /// Human-readable live state for troubleshooting, with no request URLs or credentials.
    pub fn diagnostic(&self) -> String {
        let status = self.error.as_deref().map_or_else(
            || {
                if self.session_pending {
                    "Requesting session"
                } else if self.session.is_none() {
                    "Waiting for session"
                } else if self.viewport_pending {
                    "Requesting attribution"
                } else if self.attribution.is_none() {
                    "Waiting for attribution"
                } else if !self.pending.is_empty() {
                    "Downloading tiles"
                } else {
                    "Ready"
                }
            },
            |error| error,
        );
        format!(
            "Status: {status}\nCurrent tiles: {} visible, {} loaded, {} pending, {} queued, {} failed\nSession total: {} requests, {} tiles loaded",
            self.visible_count,
            self.entries.len(),
            self.pending.len(),
            self.queued.len(),
            self.failed.len(),
            self.tile_requests_this_session,
            self.tiles_loaded_this_session,
        )
    }

    pub fn begin_frame(&mut self, ctx: &Context) {
        self.frame = self.frame.wrapping_add(1);
        while let Ok(completed) = self.rx.try_recv() {
            match completed {
                GoogleCompleted::Session(generation, result) if generation == self.generation => {
                    self.session_pending = false;
                    let failure = match result {
                        Ok(response) if !response.ok => Some(format!("Google session request failed (HTTP {}).", response.status)),
                        Ok(response) if response.bytes.len() > 32 * 1024 => Some("Google session response was too large.".to_owned()),
                        Ok(response) => match serde_json::from_slice::<GoogleSessionResponse>(&response.bytes) {
                            Ok(value) if safe_session_token(&value.session) => {
                                self.session = Some((value.session, Instant::now() + GOOGLE_SESSION_LIFETIME));
                                self.session_retry = None;
                                self.error = None;
                                None
                            }
                            _ => Some(format!("Google session response was invalid (HTTP {}).", response.status)),
                        },
                        Err(_) => Some("Google session request failed before an HTTP response (network or connection error).".to_owned()),
                    };
                    if let Some(failure) = failure {
                        self.session_retry = Some(Instant::now() + GOOGLE_RETRY_DELAY);
                        self.error = Some(failure);
                    }
                }
                GoogleCompleted::Viewport(generation, result) if generation == self.generation => {
                    self.viewport_pending = false;
                    let failure = match result {
                        Ok(response) if !response.ok => Some(format!("Google attribution request failed (HTTP {}).", response.status)),
                        Ok(response) if response.bytes.len() > 64 * 1024 => Some("Google attribution response was too large.".to_owned()),
                        Ok(response) => {
                            let status = response.status;
                            match serde_json::from_slice::<GoogleViewportResponse>(&response.bytes) {
                                Ok(viewport) => {
                                    let value = viewport.copyright.trim();
                                    if !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control) {
                                        self.attribution = Some(value.to_owned());
                                        self.viewport_retry = None;
                                        self.error = None;
                                        None
                                    } else {
                                        Some(format!("Google attribution response had no usable copyright (HTTP {status})."))
                                    }
                                }
                                Err(_) => Some(format!("Google attribution response was invalid (HTTP {status}).")),
                            }
                        }
                        Err(_) => Some("Google attribution request failed before an HTTP response (network or connection error).".to_owned()),
                    };
                    if let Some(failure) = failure {
                        self.attribution = None;
                        self.viewport_retry = Some(Instant::now() + GOOGLE_RETRY_DELAY);
                        self.error = Some(failure);
                    }
                }
                GoogleCompleted::Tile(generation, id, result) if generation == self.generation => {
                    self.pending.remove(&id);
                    match result {
                        Ok(response) if !response.ok => {
                            self.failed.insert(id, Instant::now());
                            self.error = Some(format!("Google tile request failed (HTTP {}).", response.status));
                        }
                        Ok(response) if response.bytes.len() > MAX_TILE_BYTES => {
                            self.failed.insert(id, Instant::now());
                            self.error = Some("Google tile response was too large.".to_owned());
                        }
                        Ok(response) => {
                            let status = response.status;
                            if let Some(expires_at) = google_cache_expiry(&response)
                                && let Some(texture) = decode_google_texture(ctx, id, &response.bytes)
                            {
                                self.entries.insert(id, Entry { texture, used: self.frame, expires_at: Some(expires_at) });
                                self.failed.remove(&id);
                                self.tiles_loaded_this_session = self.tiles_loaded_this_session.saturating_add(1);
                            } else {
                                self.failed.insert(id, Instant::now());
                                self.error = Some(format!("Google tile response was unsupported or not cacheable (HTTP {status})."));
                            }
                        }
                        Err(_) => {
                            self.failed.insert(id, Instant::now());
                            self.error = Some("Google tile request failed before an HTTP response (network or connection error).".to_owned());
                        }
                    }
                }
                _ => {}
            }
        }
        if self.entries.len() > MAX_TEXTURES {
            let mut by_age: Vec<_> = self.entries.iter().map(|(id, entry)| (*id, entry.used)).collect();
            by_age.sort_unstable_by_key(|(_, used)| *used);
            for (id, _) in by_age.into_iter().take(self.entries.len() - MAX_TEXTURES) {
                self.entries.remove(&id);
            }
        }
        self.entries.retain(|_, entry| entry.expires_at.is_none_or(|expires| expires > Instant::now()));
        self.failed.retain(|_, at| at.elapsed() < RETRY_DELAY);
    }

    pub fn texture(&mut self, id: TileId) -> Option<&TextureHandle> {
        if self.entries.get(&id).and_then(|entry| entry.expires_at).is_some_and(|expires| expires <= Instant::now()) {
            self.entries.remove(&id);
        }
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.used = self.frame;
            return Some(&entry.texture);
        }
        if !self.pending.contains(&id) && !self.queued.contains(&id) && !self.failed.contains_key(&id) && self.queued.len() < 256 {
            self.queued.push_back(id);
        }
        None
    }

    pub fn request_visible(&mut self, ctx: &Context, visible: &HashSet<TileId>, zoom: Zoom, bounds: TileBounds) {
        self.visible_count = visible.len();
        let Some(api_key) = self.api_key.as_deref() else { return };
        let provider = match GoogleMapsSatellite::new(api_key, None) {
            Ok(provider) => provider,
            Err(_) => return,
        };
        let generation = self.generation;
        let now = Instant::now();
        if self.session.as_ref().is_some_and(|(_, expires)| *expires <= now) {
            self.session = None;
            self.viewport_key = None;
            self.viewport_retry = None;
            self.attribution = None;
            self.error = None;
        }
        if self.session.is_none() && !self.session_pending && self.session_retry.is_none_or(|retry| retry <= now) {
            self.session_pending = true;
            self.session_retry = None;
            let request = ehttp::Request::post(provider.session_url(), GOOGLE_HYBRID_SESSION_BODY.to_vec())
                .with_header("Content-Type", "application/json")
                .with_timeout(Some(Duration::from_secs(20)));
            let tx = self.tx.clone();
            let ctx = ctx.clone();
            ehttp::fetch(request, move |result| {
                let _ = tx.send(GoogleCompleted::Session(generation, result));
                ctx.request_repaint();
            });
            return;
        }
        let Some((session, _)) = self.session.as_ref() else { return };
        // Tile URLs require the session token. The provider above intentionally has no token
        // because it is also used to construct the create-session URL.
        let tile_provider = match GoogleMapsSatellite::new(api_key, Some(session)) {
            Ok(provider) => provider,
            Err(_) => {
                self.error = Some("CADCraft could not prepare Google tile requests (invalid session).".to_owned());
                return;
            }
        };
        let viewport_key = format!("{}:{:.5}:{:.5}:{:.5}:{:.5}", zoom.value(), bounds.west, bounds.south, bounds.east, bounds.north);
        if (self.viewport_key.as_deref() != Some(&viewport_key) || self.viewport_retry.is_some_and(|retry| retry <= now)) && !self.viewport_pending {
            self.viewport_key = Some(viewport_key);
            self.viewport_pending = true;
            self.viewport_retry = None;
            self.attribution = None;
            if let Ok(url) = provider.viewport_url(zoom, bounds, session) {
                let request = ehttp::Request::get(url).with_timeout(Some(Duration::from_secs(20)));
                let tx = self.tx.clone();
                let ctx = ctx.clone();
                ehttp::fetch(request, move |result| {
                    let _ = tx.send(GoogleCompleted::Viewport(generation, result));
                    ctx.request_repaint();
                });
            } else {
                self.viewport_pending = false;
            }
            return;
        }
        if self.attribution.is_none() {
            return;
        }
        self.queued.retain(|id| visible.contains(id));
        while self.pending.len() < MAX_REQUESTS {
            let Some(id) = self.queued.pop_front() else { break };
            if !visible.contains(&id) || self.entries.contains_key(&id) || !self.pending.insert(id) {
                continue;
            }
            let Ok(url) = tile_provider.tile_url(id) else {
                self.pending.remove(&id);
                self.failed.insert(id, now);
                self.error = Some("CADCraft could not prepare a Google tile request.".to_owned());
                continue;
            };
            let request = ehttp::Request::get(url).with_timeout(Some(Duration::from_secs(20)));
            self.tile_requests_this_session = self.tile_requests_this_session.saturating_add(1);
            let tx = self.tx.clone();
            let ctx = ctx.clone();
            ehttp::fetch(request, move |result| {
                let _ = tx.send(GoogleCompleted::Tile(generation, id, result));
                ctx.request_repaint();
            });
        }
    }
}

// Esri's documented composition is the World Imagery raster service beneath the imagery-label
// raster tiles from Static Basemap Tiles. Both stay memory-only and use the same XYZ footprint.
const ESRI_IMAGERY_ATTRIBUTION: &str = "Powered by Esri · Imagery: Esri, Vantor, Earthstar Geographics, and the GIS User Community";
const ESRI_LABEL_ATTRIBUTION: &str = "Labels: Esri, TomTom, Garmin, FAO, NOAA, USGS, © OpenStreetMap contributors, and the GIS User Community";
const ESRI_RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum EsriLayer {
    Imagery,
    Labels,
}

enum EsriCompleted {
    Tile(TileId, EsriLayer, ehttp::Result<ehttp::Response>),
}

struct EsriEntry {
    imagery: Option<TextureHandle>,
    labels: Option<TextureHandle>,
    used: u64,
}

pub struct EsriTileCache {
    api_key: Option<String>,
    key_remembered: bool,
    entries: HashMap<TileId, EsriEntry>,
    queued: VecDeque<(TileId, EsriLayer)>,
    pending: HashSet<(TileId, EsriLayer)>,
    failed: HashMap<(TileId, EsriLayer), Instant>,
    tx: Sender<EsriCompleted>,
    rx: Receiver<EsriCompleted>,
    frame: u64,
    visible_count: usize,
    requests_this_session: u64,
    tiles_loaded_this_session: u64,
    error: Option<String>,
}

impl Default for EsriTileCache {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            api_key: None,
            key_remembered: false,
            entries: HashMap::new(),
            queued: VecDeque::new(),
            pending: HashSet::new(),
            failed: HashMap::new(),
            tx,
            rx,
            frame: 0,
            visible_count: 0,
            requests_this_session: 0,
            tiles_loaded_this_session: 0,
            error: None,
        }
    }
}

impl EsriTileCache {
    pub fn set_api_key(&mut self, key: Option<String>) -> Result<(), String> {
        let key = key.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
        if let Some(value) = key.as_deref() {
            EsriWorldImagery::new(value).map_err(|_| "ArcGIS API key contains unsupported characters".to_owned())?;
        }
        if key == self.api_key {
            return Ok(());
        }
        self.api_key = key;
        self.entries.clear();
        self.queued.clear();
        self.pending.clear();
        self.failed.clear();
        self.visible_count = 0;
        self.requests_this_session = 0;
        self.tiles_loaded_this_session = 0;
        self.error = None;
        Ok(())
    }

    /// Restore a key explicitly saved in this OS user's credential store.
    pub fn load_remembered_key(&mut self) -> Result<bool, &'static str> {
        let Some(key) = read_saved_key(ESRI_KEYRING_USER)? else {
            self.key_remembered = false;
            return Ok(false);
        };
        self.set_api_key(Some(key)).map_err(|_| "The saved ArcGIS key is invalid")?;
        self.key_remembered = true;
        Ok(true)
    }

    pub fn key_is_remembered(&self) -> bool {
        self.key_remembered
    }

    pub fn remember_current_key(&mut self) -> Result<(), &'static str> {
        let key = self.api_key.as_deref().ok_or("Enter an ArcGIS API key first")?;
        write_saved_key(ESRI_KEYRING_USER, key)?;
        self.key_remembered = true;
        Ok(())
    }

    pub fn forget_remembered_key(&mut self) -> Result<(), &'static str> {
        delete_saved_key(ESRI_KEYRING_USER)?;
        self.key_remembered = false;
        Ok(())
    }

    pub fn can_remember_key() -> bool {
        GoogleTileCache::can_remember_key()
    }

    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    pub fn attribution(&self) -> String {
        format!("{ESRI_IMAGERY_ATTRIBUTION} · {ESRI_LABEL_ATTRIBUTION}")
    }

    pub fn has_error(&self) -> bool {
        self.error.is_some()
    }

    pub fn has_complete_tile(&self, id: TileId) -> bool {
        self.entries.get(&id).is_some_and(|entry| entry.imagery.is_some() && entry.labels.is_some())
    }

    pub fn request_status(&self) -> &'static str {
        if !self.pending.is_empty() { "Esri · downloading imagery and labels" } else { "Esri · waiting for imagery and labels" }
    }

    pub fn diagnostic(&self) -> String {
        let status = self.error.as_deref().unwrap_or(if self.api_key.is_none() {
            "API key required"
        } else if !self.pending.is_empty() {
            "Downloading tiles"
        } else {
            "Ready"
        });
        format!(
            "Status: {status}\nCurrent tiles: {} visible, {} loaded, {} pending, {} queued, {} failed\nSession total: {} requests, {} tiles loaded",
            self.visible_count,
            self.entries.values().filter(|entry| entry.imagery.is_some() && entry.labels.is_some()).count(),
            self.pending.len(),
            self.queued.len(),
            self.failed.len(),
            self.requests_this_session,
            self.tiles_loaded_this_session,
        )
    }

    pub fn begin_frame(&mut self, ctx: &Context) {
        self.frame = self.frame.wrapping_add(1);
        while let Ok(EsriCompleted::Tile(id, layer, result)) = self.rx.try_recv() {
            self.pending.remove(&(id, layer));
            match result {
                Ok(response) if response.ok && response.bytes.len() <= MAX_TILE_BYTES => {
                    if let Some(texture) = decode_esri_texture(ctx, id, layer, &response.bytes) {
                        let entry = self.entries.entry(id).or_insert(EsriEntry { imagery: None, labels: None, used: self.frame });
                        entry.used = self.frame;
                        match layer {
                            EsriLayer::Imagery => entry.imagery = Some(texture),
                            EsriLayer::Labels => entry.labels = Some(texture),
                        }
                        self.failed.remove(&(id, layer));
                        if entry.imagery.is_some() && entry.labels.is_some() {
                            self.tiles_loaded_this_session = self.tiles_loaded_this_session.saturating_add(1);
                        }
                    } else {
                        self.failed.insert((id, layer), Instant::now());
                        self.error = Some(esri_non_image_response(layer, &response));
                    }
                }
                Ok(response) => {
                    self.failed.insert((id, layer), Instant::now());
                    self.error = Some(format!("Esri {} tile request failed (HTTP {}).", esri_layer_name(layer), response.status));
                }
                Err(_) => {
                    self.failed.insert((id, layer), Instant::now());
                    self.error = Some(format!("Esri {} tile request failed before an HTTP response.", esri_layer_name(layer)));
                }
            }
        }
        if self.entries.len() > MAX_TEXTURES {
            let mut by_age: Vec<_> = self.entries.iter().map(|(id, entry)| (*id, entry.used)).collect();
            by_age.sort_unstable_by_key(|(_, used)| *used);
            for (id, _) in by_age.into_iter().take(self.entries.len() - MAX_TEXTURES) {
                self.entries.remove(&id);
            }
        }
        self.failed.retain(|_, at| at.elapsed() < ESRI_RETRY_DELAY);
    }

    pub fn textures(&mut self, id: TileId) -> (Option<TextureHandle>, Option<TextureHandle>) {
        let entry = self.entries.entry(id).or_insert(EsriEntry { imagery: None, labels: None, used: self.frame });
        entry.used = self.frame;
        (entry.imagery.clone(), entry.labels.clone())
    }

    pub fn request_visible(&mut self, ctx: &Context, visible: &HashSet<TileId>) {
        self.visible_count = visible.len();
        self.queued.retain(|(id, _)| visible.contains(id));
        let Some(key) = self.api_key.as_deref() else { return };
        let Ok(provider) = EsriWorldImagery::new(key) else { return };
        for id in visible {
            for layer in [EsriLayer::Imagery, EsriLayer::Labels] {
                let already_loaded = self.entries.get(id).is_some_and(|entry| match layer {
                    EsriLayer::Imagery => entry.imagery.is_some(),
                    EsriLayer::Labels => entry.labels.is_some(),
                });
                let request_id = (*id, layer);
                if !already_loaded
                    && !self.pending.contains(&request_id)
                    && !self.queued.contains(&request_id)
                    && !self.failed.contains_key(&request_id)
                    && self.queued.len() < 512
                {
                    self.queued.push_back(request_id);
                }
            }
        }
        while self.pending.len() < MAX_REQUESTS {
            let Some((id, layer)) = self.queued.pop_front() else { break };
            if !visible.contains(&id) || !self.pending.insert((id, layer)) {
                continue;
            }
            let url = match layer {
                EsriLayer::Imagery => provider.imagery_tile_url(id),
                EsriLayer::Labels => provider.labels_tile_url(id),
            };
            let Ok(url) = url else {
                self.pending.remove(&(id, layer));
                self.failed.insert((id, layer), Instant::now());
                continue;
            };
            let request = ehttp::Request::get(url).with_timeout(Some(Duration::from_secs(20)));
            let request = if layer == EsriLayer::Labels {
                let authorization = format!("Bearer {key}");
                request.with_header("Authorization", &authorization)
            } else {
                request
            };
            self.requests_this_session = self.requests_this_session.saturating_add(1);
            let tx = self.tx.clone();
            let ctx = ctx.clone();
            ehttp::fetch(request, move |result| {
                let _ = tx.send(EsriCompleted::Tile(id, layer, result));
                ctx.request_repaint();
            });
        }
    }
}

fn esri_layer_name(layer: EsriLayer) -> &'static str {
    match layer {
        EsriLayer::Imagery => "imagery",
        EsriLayer::Labels => "label",
    }
}

fn esri_non_image_response(layer: EsriLayer, response: &ehttp::Response) -> String {
    let code = serde_json::from_slice::<serde_json::Value>(&response.bytes)
        .ok()
        .and_then(|body| body.pointer("/error/code").and_then(serde_json::Value::as_u64));
    if let Some(code) = code {
        let detail = match code {
            498 => "API key invalid or expired",
            499 => "token missing or not accepted by this endpoint",
            403 => "check API key privileges",
            _ => "check API key and service access",
        };
        return format!("Esri {} returned ArcGIS error {code}: {detail}.", esri_layer_name(layer));
    }
    let content_type = response.headers.get("content-type").and_then(|value| value.split(';').next()).unwrap_or("unknown content type");
    format!("Esri {} response was not an image (HTTP {}, {content_type}).", esri_layer_name(layer), response.status)
}

fn decode_esri_texture(ctx: &Context, id: TileId, layer: EsriLayer, bytes: &[u8]) -> Option<TextureHandle> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    if !(256..=512).contains(&width) || !(256..=512).contains(&height) {
        return None;
    }
    let rgba = image::load_from_memory(bytes).ok()?.to_rgba8();
    let pixels = ColorImage::from_rgba_unmultiplied([width as usize, height as usize], rgba.as_raw());
    let name = match layer {
        EsriLayer::Imagery => "imagery",
        EsriLayer::Labels => "labels",
    };
    Some(ctx.load_texture(format!("esri-{name}-{}-{}-{}", id.zoom().value(), id.x(), id.y()), pixels, TextureOptions::LINEAR))
}

#[cfg(all(not(target_arch = "wasm32"), any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn read_saved_key(user: &str) -> Result<Option<String>, &'static str> {
    let entry = keyring::Entry::new(GOOGLE_KEYRING_SERVICE, user).map_err(|_| "Could not access the OS credential store")?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("Could not read the saved map-provider key from the OS credential store"),
    }
}

#[cfg(not(all(not(target_arch = "wasm32"), any(target_os = "windows", target_os = "macos", target_os = "linux"))))]
fn read_saved_key(_user: &str) -> Result<Option<String>, &'static str> {
    Ok(None)
}

#[cfg(all(not(target_arch = "wasm32"), any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn write_saved_key(user: &str, key: &str) -> Result<(), &'static str> {
    let entry = keyring::Entry::new(GOOGLE_KEYRING_SERVICE, user).map_err(|_| "Could not access the OS credential store")?;
    entry.set_password(key).map_err(|_| "Could not save the key in the OS credential store")
}

#[cfg(not(all(not(target_arch = "wasm32"), any(target_os = "windows", target_os = "macos", target_os = "linux"))))]
fn write_saved_key(_user: &str, _key: &str) -> Result<(), &'static str> {
    Err("Remembering keys is unavailable on this platform")
}

#[cfg(all(not(target_arch = "wasm32"), any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn delete_saved_key(user: &str) -> Result<(), &'static str> {
    let entry = keyring::Entry::new(GOOGLE_KEYRING_SERVICE, user).map_err(|_| "Could not access the OS credential store")?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("Could not remove the saved key from the OS credential store"),
    }
}

#[cfg(not(all(not(target_arch = "wasm32"), any(target_os = "windows", target_os = "macos", target_os = "linux"))))]
fn delete_saved_key(_user: &str) -> Result<(), &'static str> {
    Ok(())
}

#[derive(serde::Deserialize)]
struct GoogleSessionResponse {
    session: String,
}

#[derive(serde::Deserialize)]
struct GoogleViewportResponse {
    copyright: String,
}

fn safe_session_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn google_cache_expiry(response: &ehttp::Response) -> Option<Instant> {
    let control = response.headers.get("cache-control")?;
    if control.split(',').any(|part| matches!(part.trim().to_ascii_lowercase().as_str(), "no-store" | "no-cache")) {
        return None;
    }
    let max_age = control.split(',').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        name.trim().eq_ignore_ascii_case("max-age").then(|| value.trim().trim_matches('"').parse::<u64>().ok()).flatten()
    })?;
    if max_age == 0 {
        return None;
    }
    Instant::now().checked_add(Duration::from_secs(max_age.min(30 * 24 * 60 * 60)))
}

fn decode_google_texture(ctx: &Context, id: TileId, bytes: &[u8]) -> Option<TextureHandle> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    if width == 0 || height == 0 || width > 512 || height > 512 {
        return None;
    }
    let rgba = image::load_from_memory(bytes).ok()?.to_rgba8();
    let pixels = ColorImage::from_rgba_unmultiplied([width as usize, height as usize], rgba.as_raw());
    Some(ctx.load_texture(format!("google-satellite-{}-{}-{}", id.zoom().value(), id.x(), id.y()), pixels, TextureOptions::LINEAR))
}

#[cfg(test)]
mod google_tests {
    use super::*;

    fn response(cache_control: Option<&str>) -> ehttp::Response {
        ehttp::Response {
            url: "https://tile.googleapis.com/2dtiles".to_owned(),
            ok: true,
            status: 200,
            status_text: "OK".to_owned(),
            headers: cache_control.map_or_else(ehttp::Headers::default, |value| ehttp::Headers::new(&[("cache-control", value)])),
            bytes: Vec::new(),
        }
    }

    #[test]
    fn google_tile_cache_obeys_cache_control_and_requires_freshness() {
        assert!(google_cache_expiry(&response(Some("public, max-age=60"))).is_some());
        assert!(google_cache_expiry(&response(Some("no-store, max-age=60"))).is_none());
        assert!(google_cache_expiry(&response(Some("no-cache, max-age=60"))).is_none());
        assert!(google_cache_expiry(&response(Some("public, max-age=0"))).is_none());
        assert!(google_cache_expiry(&response(None)).is_none());
    }

    #[test]
    fn google_request_diagnostics_report_stage_and_status_without_response_body() {
        let mut cache = GoogleTileCache::default();
        assert!(cache.diagnostic().contains("Session total: 0 requests, 0 tiles loaded"));
        assert_eq!(cache.diagnostic().lines().count(), 3);
        cache.session_pending = true;
        assert!(cache.request_status().contains("satellite session"));

        let mut failed = response(None);
        failed.ok = false;
        failed.status = 403;
        failed.bytes = b"secret-api-key-and-session-token".to_vec();
        assert!(cache.tx.send(GoogleCompleted::Session(cache.generation, Ok(failed))).is_ok());
        cache.begin_frame(&Context::default());

        let diagnostic = cache.error_message().unwrap_or_default();
        assert_eq!(diagnostic, "Google session request failed (HTTP 403).");
        assert!(!diagnostic.contains("secret"));
        cache.tile_requests_this_session = 3;
        cache.tiles_loaded_this_session = 2;
        assert!(cache.diagnostic().contains("Session total: 3 requests, 2 tiles loaded"));
    }

    #[test]
    fn google_satellite_tile_provider_includes_the_active_session() {
        let zoom = Zoom::new(3);
        assert_eq!(zoom.as_ref().map(|value| value.value()), Ok(3));
        let tile = zoom.ok().and_then(|zoom| TileId::new(zoom, 4, 2).ok());
        assert!(tile.is_some());
        let Some(tile) = tile else { return };
        let provider = GoogleMapsSatellite::new("test_key", Some("test_session"));
        assert!(provider.is_ok());
        let url = provider.ok().and_then(|provider| provider.tile_url(tile).ok());
        assert!(url.as_deref().is_some_and(|url| url.contains("session=test_session")));
    }

    #[test]
    fn google_satellite_session_requests_google_composited_hybrid_tiles() {
        let body: Result<serde_json::Value, _> = serde_json::from_slice(GOOGLE_HYBRID_SESSION_BODY);
        assert!(body.is_ok());
        let Some(body) = body.ok() else { return };
        assert_eq!(body["mapType"], "satellite");
        assert_eq!(body["layerTypes"][0], "layerRoadmap");
        assert_eq!(body["overlay"], false);
    }
}

#[cfg(test)]
mod esri_tests {
    use super::*;

    fn esri_response(content_type: &str, bytes: &[u8]) -> ehttp::Response {
        ehttp::Response {
            url: "https://static-map-tiles-api.arcgis.com/tile".to_owned(),
            ok: true,
            status: 200,
            status_text: "OK".to_owned(),
            headers: ehttp::Headers::new(&[("content-type", content_type)]),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn esri_diagnostic_reports_current_and_session_tile_statistics_in_three_lines() {
        let mut cache = EsriTileCache::default();
        assert_eq!(cache.diagnostic().lines().count(), 3);
        assert!(cache.diagnostic().contains("Session total: 0 requests, 0 tiles loaded"));

        // Esri fetches imagery and its transparent label overlay as separate requests.
        cache.visible_count = 20;
        cache.requests_this_session = 136;
        cache.tiles_loaded_this_session = 68;
        let diagnostic = cache.diagnostic();
        assert!(diagnostic.contains("Current tiles: 20 visible"));
        assert!(diagnostic.contains("Session total: 136 requests, 68 tiles loaded"));
        assert_eq!(diagnostic.lines().count(), 3);
    }

    #[test]
    fn esri_non_image_diagnostics_report_arcgis_error_code_without_response_body() {
        let response = esri_response("application/json; charset=utf-8", br#"{"error":{"code":403,"message":"denied"}}"#);
        let diagnostic = esri_non_image_response(EsriLayer::Labels, &response);
        assert_eq!(diagnostic, "Esri label returned ArcGIS error 403: check API key privileges.");
        assert!(!diagnostic.contains("denied"));

        let response = esri_response("application/json", br#"{"error":{"code":499}}"#);
        assert_eq!(
            esri_non_image_response(EsriLayer::Imagery, &response),
            "Esri imagery returned ArcGIS error 499: token missing or not accepted by this endpoint."
        );

        let response = esri_response("text/html", b"not an image");
        let diagnostic = esri_non_image_response(EsriLayer::Imagery, &response);
        assert_eq!(diagnostic, "Esri imagery response was not an image (HTTP 200, text/html).");
    }
}
