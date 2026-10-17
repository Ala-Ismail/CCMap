//! View-only OpenStreetMap backdrop painting.

use std::collections::HashSet;

use cadcraft_engine::Session;
use cadcraft_geom::Vec2;
use cadcraft_map::{MAX_VISIBLE_TILES, MapMode, TileId, Zoom, from_wgs84, tile_at, tile_bounds, to_wgs84};
use egui::epaint::Vertex;
use egui::{Color32, FontId, Mesh, Painter, Rect, Shape, TextureHandle, pos2};

use crate::canvas::Xf;
use crate::map_tiles::TileCache;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintStatus {
    Off,
    GeoreferenceRequired,
    Tiles { count: usize, zoom: u8 },
    InvalidRegistration,
}

pub fn paint_background(session: &Session, cache: &mut TileCache, painter: &Painter, xf: &Xf, clip: Rect) -> PaintStatus {
    let Ok(state) = session.state() else { return PaintStatus::Off };
    let map = state.map;
    if map.mode != MapMode::Street {
        return PaintStatus::Off;
    }
    let Some(registration) = map.registration else { return PaintStatus::GeoreferenceRequired };
    cache.begin_frame(painter.ctx());
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
    let mut zoom_value = 19_u8;
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
            let texture = cache.texture(painter.ctx(), tile).cloned();
            if paint_tile(painter, xf, registration, tile, texture.as_ref(), alpha) {
                count += 1;
            }
        }
    }
    #[cfg(not(test))]
    cache.request_visible(painter.ctx(), &visible);
    #[cfg(test)]
    let _ = visible;
    PaintStatus::Tiles { count, zoom: zoom.value() }
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

pub fn paint_overlay(painter: &Painter, clip: Rect, status: PaintStatus) {
    let message = match status {
        PaintStatus::Off => return,
        PaintStatus::GeoreferenceRequired => "Map: georeference required",
        PaintStatus::InvalidRegistration => "Map: registration cannot be displayed",
        PaintStatus::Tiles { .. } => "© OpenStreetMap contributors · openstreetmap.org/copyright",
    };
    // The command line floats over the bottom of the canvas; keep attribution clear of it.
    let at = pos2(clip.left() + 8.0, clip.bottom() - 48.0);
    let galley = painter.layout_no_wrap(message.to_owned(), FontId::proportional(11.0), Color32::WHITE);
    let label = Rect::from_min_max(pos2(at.x - 4.0, at.y - galley.size().y - 3.0), pos2(at.x + galley.size().x + 4.0, at.y + 3.0));
    painter.rect_filled(label, 2.0, Color32::from_black_alpha(190));
    painter.galley(pos2(at.x, at.y - galley.size().y), galley, Color32::WHITE);
}

#[cfg(test)]
#[allow(clippy::unreachable)]
mod tests {
    use cadcraft_engine::Session;
    use egui::{Context, LayerId, RawInput};
    use serde_json::json;

    use super::*;

    #[test]
    fn off_and_missing_registration_request_no_tiles() {
        let mut session = Session::new();
        let context = Context::default();
        let clip = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        let xf = Xf { rect: clip, center: Vec2::ZERO, scale: 1.0 };
        let mut cache = TileCache::default();
        let mut status = PaintStatus::InvalidRegistration;
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let painter = Painter::new(ui.ctx().clone(), LayerId::background(), clip);
            status = paint_background(&session, &mut cache, &painter, &xf, clip);
        });
        output.textures_delta.clear();
        assert_eq!(status, PaintStatus::Off);
        assert!(session.execute("map.mode", &json!({"mode":"street"})).is_ok());
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let painter = Painter::new(ui.ctx().clone(), LayerId::background(), clip);
            status = paint_background(&session, &mut cache, &painter, &xf, clip);
        });
        output.textures_delta.clear();
        assert_eq!(status, PaintStatus::GeoreferenceRequired);
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
        let mut status = PaintStatus::InvalidRegistration;
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let painter = Painter::new(ui.ctx().clone(), LayerId::background(), clip);
            status = paint_background(&session, &mut cache, &painter, &xf, clip);
        });
        output.textures_delta.clear();
        let PaintStatus::Tiles { count, .. } = status else {
            unreachable!("registered map must paint tiles");
        };
        assert!(count > 0 && count <= MAX_VISIBLE_TILES);
    }
}
