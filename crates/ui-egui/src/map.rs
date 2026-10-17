//! View-only map backdrop painting.
//!
//! This first vertical slice uses deterministic mock tiles. It proves registration, tile
//! selection, painter ordering, and commands without network or decoded-image failure modes.

use cadcraft_engine::Session;
use cadcraft_geom::Vec2;
use cadcraft_map::{MAX_VISIBLE_TILES, MapMode, TileId, Zoom, from_wgs84, tile_at, tile_bounds, to_wgs84};
use egui::{Align2, Color32, FontId, Painter, Rect, Shape, Stroke, pos2};

use crate::canvas::Xf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintStatus {
    Off,
    GeoreferenceRequired,
    Tiles { count: usize, zoom: u8 },
    InvalidRegistration,
}

pub fn paint_background(session: &Session, painter: &Painter, xf: &Xf, clip: Rect) -> PaintStatus {
    let Ok(state) = session.state() else { return PaintStatus::Off };
    let map = state.map;
    if map.mode == MapMode::Off {
        return PaintStatus::Off;
    }
    let Some(registration) = map.registration else { return PaintStatus::GeoreferenceRequired };
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

    let mut zoom_value = 15_u8;
    let (zoom, west, east, north, south) = loop {
        let Ok(zoom) = Zoom::new(zoom_value) else { return PaintStatus::InvalidRegistration };
        let ids: Vec<_> = geographic.iter().copied().map(|point| tile_at(point, zoom)).collect();
        let west = ids.iter().map(|tile| tile.x()).min().unwrap_or(0);
        let east = ids.iter().map(|tile| tile.x()).max().unwrap_or(0);
        let north = ids.iter().map(|tile| tile.y()).min().unwrap_or(0);
        let south = ids.iter().map(|tile| tile.y()).max().unwrap_or(0);
        let count = u64::from(east - west + 1).saturating_mul(u64::from(south - north + 1));
        if count <= MAX_VISIBLE_TILES as u64 || zoom_value == 0 {
            break (zoom, west, east, north, south);
        }
        zoom_value -= 1;
    };

    let alpha = (map.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut count = 0_usize;
    for y in north..=south {
        for x in west..=east {
            if count >= MAX_VISIBLE_TILES {
                return PaintStatus::Tiles { count, zoom: zoom.value() };
            }
            let Ok(tile) = TileId::new(zoom, x, y) else { continue };
            if paint_mock_tile(painter, xf, registration, tile, alpha) {
                count += 1;
            }
        }
    }
    PaintStatus::Tiles { count, zoom: zoom.value() }
}

fn paint_mock_tile(painter: &Painter, xf: &Xf, registration: cadcraft_map::Registration, tile: TileId, alpha: u8) -> bool {
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
    let even = tile.x() & 1 == tile.y() & 1;
    let base = if even { (69, 111, 78) } else { (75, 123, 88) };
    painter.add(Shape::convex_polygon(
        points.clone(),
        Color32::from_rgba_unmultiplied(base.0, base.1, base.2, alpha),
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(205, 225, 205, alpha)),
    ));
    let bounds = Rect::from_points(&points);
    if bounds.width() > 64.0 && bounds.height() > 28.0 {
        painter.text(
            bounds.center(),
            Align2::CENTER_CENTER,
            format!("{}/{}/{}", tile.zoom().value(), tile.x(), tile.y()),
            FontId::monospace(10.0),
            Color32::from_rgba_unmultiplied(245, 250, 245, alpha),
        );
    }
    true
}

pub fn paint_overlay(painter: &Painter, clip: Rect, status: PaintStatus) {
    let message = match status {
        PaintStatus::Off => return,
        PaintStatus::GeoreferenceRequired => "Map: georeference required",
        PaintStatus::InvalidRegistration => "Map: registration cannot be displayed",
        PaintStatus::Tiles { .. } => "Mock map · CADCraft deterministic tiles",
    };
    let at = pos2(clip.left() + 8.0, clip.bottom() - 8.0);
    painter.text(at, Align2::LEFT_BOTTOM, message, FontId::proportional(11.0), Color32::WHITE);
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
        let mut status = PaintStatus::InvalidRegistration;
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let painter = Painter::new(ui.ctx().clone(), LayerId::background(), clip);
            status = paint_background(&session, &painter, &xf, clip);
        });
        output.textures_delta.clear();
        assert_eq!(status, PaintStatus::Off);
        assert!(session.execute("map.mode", &json!({"mode":"street"})).is_ok());
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let painter = Painter::new(ui.ctx().clone(), LayerId::background(), clip);
            status = paint_background(&session, &painter, &xf, clip);
        });
        output.textures_delta.clear();
        assert_eq!(status, PaintStatus::GeoreferenceRequired);
    }

    #[test]
    fn registered_mock_map_paints_a_bounded_tile_set() {
        let mut session = Session::new();
        assert!(session.execute("map.mode", &json!({"mode":"street"})).is_ok());
        assert!(session.execute("map.georef.set", &json!({"crs":"EPSG:7855", "drawingAnchor":[0,0], "projectAnchor":[320000,5813000]}),).is_ok());
        let context = Context::default();
        let clip = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        let xf = Xf { rect: clip, center: Vec2::ZERO, scale: 4.0 };
        let mut status = PaintStatus::InvalidRegistration;
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let painter = Painter::new(ui.ctx().clone(), LayerId::background(), clip);
            status = paint_background(&session, &painter, &xf, clip);
        });
        output.textures_delta.clear();
        let PaintStatus::Tiles { count, .. } = status else {
            unreachable!("registered map must paint tiles");
        };
        assert!(count > 0 && count <= MAX_VISIBLE_TILES);
    }
}
