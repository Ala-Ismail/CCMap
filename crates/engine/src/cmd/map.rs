//! Provider-independent map backdrop commands.

use cadcraft_map::{Coordinate, CrsId, MapMode, Registration};
use serde_json::{Value, json};

use super::{CommandSpec, bad, point_req};
use crate::{Result, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("map.mode", "Map Mode", mode).params("{mode: off|street|satellite}").noundo(),
        CommandSpec::new("map.opacity", "Map Opacity", opacity).params("{opacity: 0..1}").noundo(),
        CommandSpec::new("map.georef.set", "Set Map Georeference", georef_set)
            .params("{crs, drawingAnchor:[x,y], projectAnchor:[x,y], scale?, rotation?}")
            .noundo(),
        CommandSpec::new("map.georef.clear", "Clear Map Georeference", georef_clear).params("{}").noundo(),
        CommandSpec::new("map.status", "Map Status", status).params("{}").noundo(),
    ]
}

fn mode(s: &mut Session, p: &Value) -> Result<Value> {
    let mode = match p.get("mode").and_then(Value::as_str).map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("off") => MapMode::Off,
        Some("street") => MapMode::Street,
        Some("satellite") => return Err(bad("map.mode", "satellite has no configured provider")),
        _ => return Err(bad("map.mode", "`mode` must be `off`, `street`, or `satellite`")),
    };
    s.state_mut()?.map.mode = mode;
    s.touch();
    status(s, &Value::Null)
}

fn opacity(s: &mut Session, p: &Value) -> Result<Value> {
    let opacity = p
        .get("opacity")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
        .ok_or_else(|| bad("map.opacity", "`opacity` must be a finite number from 0 to 1"))?;
    s.state_mut()?.map.opacity = opacity as f32;
    s.touch();
    status(s, &Value::Null)
}

fn georef_set(s: &mut Session, p: &Value) -> Result<Value> {
    let crs = match p.get("crs").and_then(Value::as_str).map(str::trim) {
        Some("EPSG:4326") => CrsId::Wgs84,
        Some("EPSG:3857") => CrsId::WebMercator,
        Some("EPSG:7855") => CrsId::Gda2020MgaZone55,
        _ => return Err(bad("map.georef.set", "`crs` must be EPSG:4326, EPSG:3857, or EPSG:7855")),
    };
    let drawing = point_req("map.georef.set", p, "drawingAnchor")?;
    let project = point_req("map.georef.set", p, "projectAnchor")?;
    let scale = optional_finite(p, "scale", 1.0, true)?;
    let rotation = optional_finite(p, "rotation", 0.0, false)?;
    let drawing = Coordinate::new(drawing.x, drawing.y).map_err(|error| bad("map.georef.set", error.to_string()))?;
    let project = Coordinate::new(project.x, project.y).map_err(|error| bad("map.georef.set", error.to_string()))?;
    let registration = Registration::new(crs, drawing, project, scale, rotation).map_err(|error| bad("map.georef.set", error.to_string()))?;
    s.state_mut()?.map.registration = Some(registration);
    s.touch();
    status(s, &Value::Null)
}

fn optional_finite(p: &Value, key: &str, default: f64, positive: bool) -> Result<f64> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite() && (!positive || *value > 0.0))
            .ok_or_else(|| bad("map.georef.set", format!("`{key}` must be a {}finite number", if positive { "positive " } else { "" }))),
    }
}

fn georef_clear(s: &mut Session, _: &Value) -> Result<Value> {
    s.state_mut()?.map.registration = None;
    s.touch();
    status(s, &Value::Null)
}

fn status(s: &mut Session, _: &Value) -> Result<Value> {
    let map = s.state()?.map;
    Ok(json!({
        "mode": match map.mode { MapMode::Off => "off", MapMode::Street => "street", MapMode::Satellite => "satellite" },
        "opacity": map.opacity,
        "provider": if map.mode == MapMode::Street { Some("openstreetmap-standard") } else { None },
        "registered": map.registration.is_some(),
        "crs": map.registration.map(|registration| registration.crs().epsg()),
        "status": if map.mode == MapMode::Off { "off" } else if map.registration.is_none() { "georeference required" } else { "ready" },
    }))
}

#[cfg(test)]
#[allow(clippy::unreachable)]
mod tests {
    use super::*;

    #[test]
    fn map_commands_validate_and_report_state() {
        let mut session = Session::new();
        assert!(session.execute("map.mode", &json!({"mode":"street"})).is_ok());
        let status = match session.execute("map.status", &json!({})) {
            Ok(value) => value,
            Err(error) => unreachable!("status must succeed: {error}"),
        };
        assert_eq!(status["status"], "georeference required");
        assert!(session.execute("map.opacity", &json!({"opacity": f64::NAN})).is_err());
        assert!(session.execute("map.opacity", &json!({"opacity": 1.1})).is_err());
        assert!(
            session
                .execute(
                    "map.georef.set",
                    &json!({"crs":"EPSG:7855", "drawingAnchor":[0,0], "projectAnchor":[321000,5812000], "scale":1.0, "rotation":0.0}),
                )
                .is_ok()
        );
        let status = match session.execute("map.status", &json!({})) {
            Ok(value) => value,
            Err(error) => unreachable!("status must succeed: {error}"),
        };
        assert_eq!(status["status"], "ready");
        assert_eq!(status["crs"], "EPSG:7855");
    }
}
