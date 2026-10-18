//! View-only OpenStreetMap backdrop painting.

use std::collections::HashSet;

use cadcraft_engine::Session;
use cadcraft_geom::Vec2;
use cadcraft_map::{MAX_VISIBLE_TILES, MapMode, TileId, Zoom, from_wgs84, tile_at, tile_bounds, to_wgs84};
use egui::epaint::Vertex;
use egui::{Color32, FontId, Mesh, Painter, Rect, Shape, TextureHandle, pos2};
use serde_json::json;

use crate::canvas::Xf;
use crate::map_tiles::TileCache;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintStatus {
    Off,
    GeoreferenceRequired,
    GoogleKeyRequired,
    GoogleLoading,
    GoogleUnavailable,
    Tiles { count: usize, zoom: u8, provider: MapMode },
    InvalidRegistration,
}

/// Small in-app selector for the drawing CRS. Choosing an MGA zone installs an identity
/// registration, so survey coordinates remain numerically unchanged; this does not use (0, 0) as
/// a control point. Anchor-based alignment remains a separate optional command workflow.
pub fn crs_dialog(app: &mut crate::CadApp, ctx: &egui::Context, open: &mut bool) {
    let id = egui::Id::new("map_crs_selection");
    let current = app.session.state().ok().and_then(|state| state.map.registration).map(|registration| registration.crs().epsg().to_owned());
    let mut selected = ctx.data_mut(|data| data.get_temp::<String>(id)).or(current).unwrap_or_default();
    let mut apply = false;
    let mut close = false;
    egui::Window::new("Set Map CRS").id(egui::Id::new("Set Map CRS")).open(open).collapsible(false).resizable(false).default_width(320.0).show(
        ctx,
        |ui| {
            ui.label("Select the coordinate reference system already used by this drawing.");
            ui.label("Survey coordinates are preserved unchanged; (0,0) is not assumed to be a control point.");
            egui::ComboBox::from_id_salt("map_crs_combo")
                .selected_text(if selected.is_empty() { "Choose an MGA zone...".to_owned() } else { selected_label(&selected) })
                .width(255.0)
                .show_ui(ui, |ui| {
                    for datum in ["GDA2020", "GDA94"] {
                        ui.push_id(datum, |ui| {
                            ui.label(datum);
                            for crs in cadcraft_map::CrsId::MGA {
                                if crs.mga_details().is_some_and(|details| details.0 == datum) {
                                    let label = selected_label(crs.epsg());
                                    ui.selectable_value(&mut selected, crs.epsg().to_owned(), label);
                                }
                            }
                        });
                    }
                });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if ui.add_enabled(!selected.is_empty(), egui::Button::new("Use This CRS")).clicked() {
                    apply = true;
                }
            });
        },
    );
    ctx.data_mut(|data| data.insert_temp(id, selected.clone()));
    if apply {
        let _ = app.run("map.georef.set", json!({"crs": selected}));
        close = true;
    }
    if close {
        *open = false;
    }
}

/// Enter a user-owned Google Maps key, optionally saving it in the current OS user's credential
/// store. The credential is never included in the profile JSON, drawing sidecar, or repository.
pub fn google_key_dialog(app: &mut crate::CadApp, ctx: &egui::Context, open: &mut bool) {
    let id = egui::Id::new("map_google_api_key_entry");
    let remember_id = egui::Id::new("map_google_api_key_remember");
    let error_id = egui::Id::new("map_google_api_key_error");
    let mut key = ctx.data_mut(|data| data.get_temp::<String>(id)).unwrap_or_default();
    let can_remember = crate::map_tiles::GoogleTileCache::can_remember_key();
    let mut remember =
        ctx.data_mut(|data| data.get_temp::<bool>(remember_id)).unwrap_or_else(|| can_remember && app.google_map_tiles.key_is_remembered());
    let mut error = ctx.data_mut(|data| data.get_temp::<String>(error_id));
    let mut apply = false;
    let mut clear = false;
    let mut close = false;
    egui::Window::new("Google Maps Satellite")
        .id(egui::Id::new("Google Maps Satellite"))
        .open(open)
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .show(ctx, |ui| {
            ui.label("Use your Google Maps Platform API key for satellite imagery.");
            ui.label("Map Tiles API and billing must be enabled in your Google Cloud project.");
            ui.label("Restrict the key to Map Tiles API. Desktop keys are visible to this app process.");
            ui.label("Tile requests may incur charges.");
            ui.add(egui::TextEdit::singleline(&mut key).password(true).hint_text("Google Maps API key").desired_width(330.0));
            if can_remember {
                ui.checkbox(&mut remember, "Remember on this computer (OS credential store)");
                ui.label("Stored for your operating-system user only; never in CADCraft files or the repo.");
            } else {
                ui.label("Remembering is unavailable on this platform; the key is session-only.");
            }
            if let Some(message) = error.as_deref() {
                ui.colored_label(Color32::LIGHT_RED, message);
            }
            ui.label(if app.google_map_tiles.has_api_key() {
                if app.google_map_tiles.key_is_remembered() {
                    "A key is active and remembered for this user."
                } else {
                    "A key is active for this session."
                }
            } else if app.google_map_tiles.key_is_remembered() {
                "A key is saved for this user."
            } else {
                "No key configured."
            });
            ui.horizontal(|ui| {
                let action = if remember { "Use Key and Remember" } else { "Use Key This Session" };
                if ui.add_enabled(!key.trim().is_empty(), egui::Button::new(action)).clicked() {
                    apply = true;
                }
                if ui
                    .add_enabled(
                        app.google_map_tiles.has_api_key() || app.google_map_tiles.key_is_remembered(),
                        egui::Button::new("Clear and Forget Key"),
                    )
                    .clicked()
                {
                    clear = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
    if apply {
        match app.google_map_tiles.set_api_key(Some(key.trim().to_owned())) {
            Ok(()) if remember => match app.google_map_tiles.remember_current_key() {
                Ok(()) => {
                    app.ui.dialog = None;
                    *open = false;
                    key.clear();
                    error = None;
                }
                Err(message) => error = Some(message.to_owned()),
            },
            Ok(()) => match app.google_map_tiles.forget_remembered_key() {
                Ok(()) => {
                    app.ui.dialog = None;
                    *open = false;
                    key.clear();
                    error = None;
                }
                Err(message) => error = Some(message.to_owned()),
            },
            Err(message) => error = Some(message),
        }
    }
    if clear {
        match app.google_map_tiles.forget_remembered_key() {
            Ok(()) => {
                let _ = app.google_map_tiles.set_api_key(None);
                remember = false;
                key.clear();
                error = None;
            }
            Err(message) => error = Some(message.to_owned()),
        }
    }
    if close || !*open {
        key.clear();
        error = None;
        app.ui.dialog = None;
        *open = false;
    }
    ctx.data_mut(|data| {
        if key.is_empty() {
            data.remove_temp::<String>(id);
        } else {
            data.insert_temp(id, key);
        }
        if *open {
            data.insert_temp(remember_id, remember);
        } else {
            data.remove_temp::<bool>(remember_id);
        }
        if let Some(message) = error {
            data.insert_temp(error_id, message);
        } else {
            data.remove_temp::<String>(error_id);
        }
    });
}

fn selected_label(epsg: &str) -> String {
    let Some(crs) = cadcraft_map::CrsId::MGA.iter().copied().find(|crs| crs.epsg() == epsg) else { return epsg.to_owned() };
    let Some((datum, zone)) = crs.mga_details() else { return epsg.to_owned() };
    format!("{datum} / MGA zone {zone} ({epsg})")
}

pub fn paint_background(
    session: &Session,
    cache: &mut TileCache,
    google_cache: &mut crate::map_tiles::GoogleTileCache,
    painter: &Painter,
    xf: &Xf,
    clip: Rect,
) -> PaintStatus {
    let Ok(state) = session.state() else { return PaintStatus::Off };
    let map = state.map;
    if map.mode == MapMode::Off {
        return PaintStatus::Off;
    }
    if map.mode == MapMode::Satellite && !google_cache.has_api_key() {
        return PaintStatus::GoogleKeyRequired;
    }
    let Some(registration) = map.registration else { return PaintStatus::GeoreferenceRequired };
    if map.mode == MapMode::Street {
        cache.begin_frame(painter.ctx());
    } else {
        google_cache.begin_frame(painter.ctx());
    }
    let corners = [clip.left_top(), clip.right_top(), clip.left_bottom(), clip.right_bottom()];
    let geographic: Option<Vec<_>> = corners
        .into_iter()
        .map(|point| {
            let drawing = xf.to_world(point);
            let drawing = cadcraft_map::Coordinate::new(drawing.x, drawing.y).ok()?;
            let project = registration.to_project(drawing).ok()?;
            to_wgs84(registration.crs(), project).ok()
        })
        .collect();
    let Some(geographic) = geographic else { return PaintStatus::InvalidRegistration };

    // Prefer full-resolution tiles, stepping down until the visible footprint is modest.
    // This avoids fetching hundreds of tiny tiles at a broad CAD zoom.
    let mut zoom_value = if map.mode == MapMode::Satellite { 22_u8 } else { 19_u8 };
    let (zoom, west, east, north, south) = loop {
        let Ok(zoom) = Zoom::new(zoom_value) else { return PaintStatus::InvalidRegistration };
        let ids: Vec<_> = geographic.iter().copied().map(|point| tile_at(point, zoom)).collect();
        let west = ids.iter().map(|tile| tile.x()).min().unwrap_or(0);
        let east = ids.iter().map(|tile| tile.x()).max().unwrap_or(0);
        let north = ids.iter().map(|tile| tile.y()).min().unwrap_or(0);
        let south = ids.iter().map(|tile| tile.y()).max().unwrap_or(0);
        let count = u64::from(east - west + 1).saturating_mul(u64::from(south - north + 1));
        if count <= 32 || zoom_value == 0 {
            break (zoom, west, east, north, south);
        }
        zoom_value -= 1;
    };

    let alpha = (map.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut count = 0_usize;
    let mut visible = HashSet::new();
    for y in north..=south {
        for x in west..=east {
            if count >= MAX_VISIBLE_TILES {
                break;
            }
            let Ok(tile) = TileId::new(zoom, x, y) else { continue };
            visible.insert(tile);
            let texture = match map.mode {
                MapMode::Off => None,
                MapMode::Street => cache.texture(painter.ctx(), tile).cloned(),
                MapMode::Satellite => google_cache.texture(tile).cloned(),
            };
            if paint_tile(painter, xf, registration, tile, texture.as_ref(), alpha) {
                count += 1;
            }
        }
    }
    let bounds = cadcraft_map::TileBounds {
        west: geographic.iter().map(|point| point.longitude()).fold(f64::INFINITY, f64::min),
        east: geographic.iter().map(|point| point.longitude()).fold(f64::NEG_INFINITY, f64::max),
        south: geographic.iter().map(|point| point.latitude()).fold(f64::INFINITY, f64::min),
        north: geographic.iter().map(|point| point.latitude()).fold(f64::NEG_INFINITY, f64::max),
    };
    #[cfg(not(test))]
    match map.mode {
        MapMode::Off => {}
        MapMode::Street => cache.request_visible(painter.ctx(), &visible),
        MapMode::Satellite => google_cache.request_visible(painter.ctx(), &visible, zoom, bounds),
    }
    #[cfg(test)]
    let _ = (visible, bounds);
    if map.mode == MapMode::Satellite && google_cache.attribution().is_none() {
        return if google_cache.error_message().is_some() { PaintStatus::GoogleUnavailable } else { PaintStatus::GoogleLoading };
    }
    PaintStatus::Tiles { count, zoom: zoom.value(), provider: map.mode }
}

fn paint_tile(
    painter: &Painter,
    xf: &Xf,
    registration: cadcraft_map::Registration,
    tile: TileId,
    texture: Option<&TextureHandle>,
    alpha: u8,
) -> bool {
    let bounds = tile_bounds(tile);
    let geographic = [(bounds.west, bounds.north), (bounds.east, bounds.north), (bounds.east, bounds.south), (bounds.west, bounds.south)];
    let points: Option<Vec<_>> = geographic
        .into_iter()
        .map(|(longitude, latitude)| {
            let position = cadcraft_map::LonLat::new(longitude, latitude).ok()?;
            let project = from_wgs84(registration.crs(), position).ok()?;
            let drawing = registration.to_drawing(project).ok()?;
            Some(xf.to_screen(Vec2::new(drawing.x(), drawing.y())))
        })
        .collect();
    let Some(points) = points else { return false };
    if let Some(texture) = texture {
        let mut mesh = Mesh::with_texture(texture.id());
        let tint = Color32::from_white_alpha(alpha);
        for (point, uv) in points.iter().copied().zip([pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)]) {
            mesh.vertices.push(Vertex { pos: point, uv, color: tint });
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        painter.add(Shape::mesh(mesh));
    }
    true
}

pub fn paint_overlay(
    painter: &Painter,
    clip: Rect,
    status: PaintStatus,
    google_attribution: Option<&str>,
    google_request_status: &str,
    google_diagnostic: Option<&str>,
) {
    let message = match status {
        PaintStatus::Off => return,
        PaintStatus::GeoreferenceRequired => "Map: georeference required",
        PaintStatus::GoogleKeyRequired => "Satellite map: configure a Google Maps API key",
        PaintStatus::GoogleLoading => google_request_status,
        PaintStatus::GoogleUnavailable => "Google Maps satellite imagery is unavailable",
        PaintStatus::InvalidRegistration => "Map: registration cannot be displayed",
        PaintStatus::Tiles { provider: MapMode::Satellite, .. } => "Google Maps",
        PaintStatus::Tiles { provider: MapMode::Street, .. } => "© OpenStreetMap contributors · openstreetmap.org/copyright",
        PaintStatus::Tiles { provider: MapMode::Off, .. } => return,
    };
    // The command line floats over the bottom of the canvas; keep attribution clear of it.
    let at = pos2(clip.left() + 8.0, clip.bottom() - 48.0);
    let galley = painter.layout_no_wrap(message.to_owned(), FontId::proportional(11.0), Color32::WHITE);
    let label = Rect::from_min_max(pos2(at.x - 4.0, at.y - galley.size().y - 3.0), pos2(at.x + galley.size().x + 4.0, at.y + 3.0));
    painter.rect_filled(label, 2.0, Color32::from_black_alpha(190));
    painter.galley(pos2(at.x, at.y - galley.size().y), galley, Color32::WHITE);
    if let Some(attribution) =
        google_attribution.filter(|_| matches!(status, PaintStatus::GoogleLoading | PaintStatus::Tiles { provider: MapMode::Satellite, .. }))
    {
        let detail = painter.layout_no_wrap(attribution.to_owned(), FontId::proportional(10.0), Color32::WHITE);
        let detail_at = pos2(clip.right() - detail.size().x - 8.0, clip.bottom() - 48.0);
        let detail_bg = Rect::from_min_max(pos2(detail_at.x - 4.0, detail_at.y - detail.size().y - 3.0), pos2(clip.right() - 4.0, detail_at.y + 3.0));
        painter.rect_filled(detail_bg, 2.0, Color32::from_black_alpha(190));
        painter.galley(pos2(detail_at.x, detail_at.y - detail.size().y), detail, Color32::WHITE);
    }
    if let Some(diagnostic) = google_diagnostic.filter(|_| {
        matches!(
            status,
            PaintStatus::GoogleKeyRequired
                | PaintStatus::GoogleLoading
                | PaintStatus::GoogleUnavailable
                | PaintStatus::Tiles { provider: MapMode::Satellite, .. }
        )
    }) {
        // Keep the multi-line diagnostics at the top of the viewport, clear of the command bar
        // and copyright overlay. This is intentionally visible while satellite mode is active.
        let lines: Vec<_> =
            diagnostic.lines().map(|line| painter.layout_no_wrap(line.to_owned(), FontId::proportional(11.0), Color32::WHITE)).collect();
        let width = lines.iter().map(|line| line.size().x).fold(0.0_f32, f32::max);
        let height = lines.iter().map(|line| line.size().y).sum::<f32>() + 2.0 * lines.len().saturating_sub(1) as f32;
        let at = pos2(clip.left() + 10.0, clip.top() + 10.0);
        painter.rect_filled(
            Rect::from_min_size(at - egui::vec2(6.0, 5.0), egui::vec2(width + 12.0, height + 10.0)),
            3.0,
            Color32::from_black_alpha(220),
        );
        let mut y = at.y;
        for line in lines {
            let line_height = line.size().y;
            painter.galley(pos2(at.x, y), line, Color32::WHITE);
            y += line_height + 2.0;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unreachable)]
mod tests {
    use cadcraft_engine::Session;
    use egui::{Context, LayerId, RawInput};
    use serde_json::json;

    use super::*;

    #[test]
    fn set_map_crs_menu_opens_the_in_app_selector() {
        let mut app = crate::CadApp::new(Session::new(), crate::Services::default());
        assert!(app.run("ui.map.crs", serde_json::Value::Null).is_ok());
        assert_eq!(app.ui.dialog.as_deref(), Some("map_crs"));
    }

    #[test]
    fn google_provider_settings_are_runtime_only() {
        let mut app = crate::CadApp::new(Session::new(), crate::Services::default());
        assert!(app.google_map_tiles.set_api_key(Some("AIza_test_key-123".to_owned())).is_ok());
        assert!(app.google_map_tiles.has_api_key());
        assert!(!app.prefs_json().contains("AIza_test_key-123"));
        assert!(!app.profile_json().contains("AIza_test_key-123"));
        assert!(app.google_map_tiles.set_api_key(None).is_ok());
        assert!(!app.google_map_tiles.has_api_key());
        assert!(app.run("ui.map.google_key", serde_json::Value::Null).is_ok());
        assert_eq!(app.ui.dialog.as_deref(), Some("map_google_key"));
    }

    #[test]
    fn off_and_missing_registration_request_no_tiles() {
        let mut session = Session::new();
        let context = Context::default();
        let clip = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        let xf = Xf { rect: clip, center: Vec2::ZERO, scale: 1.0 };
        let mut cache = TileCache::default();
        let mut google_cache = crate::map_tiles::GoogleTileCache::default();
        let mut status = PaintStatus::InvalidRegistration;
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let painter = Painter::new(ui.ctx().clone(), LayerId::background(), clip);
            status = paint_background(&session, &mut cache, &mut google_cache, &painter, &xf, clip);
        });
        output.textures_delta.clear();
        assert_eq!(status, PaintStatus::Off);
        assert!(session.execute("map.mode", &json!({"mode":"street"})).is_ok());
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let painter = Painter::new(ui.ctx().clone(), LayerId::background(), clip);
            status = paint_background(&session, &mut cache, &mut google_cache, &painter, &xf, clip);
        });
        output.textures_delta.clear();
        assert_eq!(status, PaintStatus::GeoreferenceRequired);
        assert!(session.execute("map.mode", &json!({"mode":"satellite"})).is_ok());
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let painter = Painter::new(ui.ctx().clone(), LayerId::background(), clip);
            status = paint_background(&session, &mut cache, &mut google_cache, &painter, &xf, clip);
        });
        output.textures_delta.clear();
        assert_eq!(status, PaintStatus::GoogleKeyRequired);
    }

    #[test]
    fn registered_map_paints_a_bounded_tile_set() {
        let mut session = Session::new();
        assert!(session.execute("map.mode", &json!({"mode":"street"})).is_ok());
        assert!(session.execute("map.georef.set", &json!({"crs":"EPSG:7855", "drawingAnchor":[0,0], "projectAnchor":[320000,5813000]}),).is_ok());
        let context = Context::default();
        let clip = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        let xf = Xf { rect: clip, center: Vec2::ZERO, scale: 4.0 };
        let mut cache = TileCache::default();
        let mut google_cache = crate::map_tiles::GoogleTileCache::default();
        let mut status = PaintStatus::InvalidRegistration;
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let painter = Painter::new(ui.ctx().clone(), LayerId::background(), clip);
            status = paint_background(&session, &mut cache, &mut google_cache, &painter, &xf, clip);
        });
        output.textures_delta.clear();
        let PaintStatus::Tiles { count, .. } = status else {
            unreachable!("registered map must paint tiles");
        };
        assert!(count > 0 && count <= MAX_VISIBLE_TILES);
    }

    #[test]
    fn satellite_mode_uses_google_and_waits_for_viewport_attribution() {
        let mut session = Session::new();
        assert!(session.execute("map.mode", &json!({"mode":"satellite"})).is_ok());
        assert!(session.execute("map.georef.set", &json!({"crs":"EPSG:7855", "drawingAnchor":[0,0], "projectAnchor":[320000,5813000]})).is_ok());
        let context = Context::default();
        let clip = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        let xf = Xf { rect: clip, center: Vec2::ZERO, scale: 4.0 };
        let mut cache = TileCache::default();
        let mut google_cache = crate::map_tiles::GoogleTileCache::default();
        assert!(google_cache.set_api_key(Some("AIza_test_key-123".to_owned())).is_ok());
        let mut status = PaintStatus::Off;
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let painter = Painter::new(ui.ctx().clone(), LayerId::background(), clip);
            status = paint_background(&session, &mut cache, &mut google_cache, &painter, &xf, clip);
        });
        output.textures_delta.clear();
        assert_eq!(status, PaintStatus::GoogleLoading);
        assert_eq!(google_cache.attribution(), None);
    }
}
