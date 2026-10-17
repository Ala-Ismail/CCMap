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

use cadcraft_map::{OpenStreetMapStandard, TileId, TileProvider};
use egui::{ColorImage, Context, TextureHandle, TextureOptions};

const MAX_REQUESTS: usize = 4;
const MAX_TEXTURES: usize = 96;
const MAX_TILE_BYTES: usize = 1024 * 1024;
const RETRY_DELAY: Duration = Duration::from_secs(60);

type Completed = (TileId, ehttp::Result<ehttp::Response>);

struct Entry {
    texture: TextureHandle,
    used: u64,
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
                        self.entries.insert(id, Entry { texture, used: self.frame });
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
            self.entries.insert(id, Entry { texture, used: self.frame });
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
