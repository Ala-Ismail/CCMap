//! Provider-independent map backdrop commands.

use cadcraft_map::{Coordinate, CrsId, MapMode, Registration};
use serde_json::{Value, json};

use super::{CommandSpec, bad, point_req};
use crate::{Accept, Input, Interactive, Prompt, Result, Session, Step};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("map.mode", "Map Mode", mode).params("{mode: off|street|satellite}").noundo(),
        CommandSpec::new("map.opacity", "Map Opacity", opacity).params("{opacity: 0..1}").noundo(),
        CommandSpec::new("map.georef.set", "Set Map Georeference", georef_set)
            .params("{crs, drawingAnchor?:[x,y], projectAnchor?:[x,y], scale?, rotation?}; omit anchors to preserve drawing coordinates")
            .noundo(),
        CommandSpec::new("map.georef.align", "Align Map to Drawing", georef_align)
            .menu(&["View", "Map", "Align Drawing to Map..."])
            .params("{drawingAnchor:[x,y], latLongAnchor?:[latitude,longitude], projectAnchor?:[easting,northing], secondDrawingAnchor?:[x,y], secondLatLongAnchor?:[latitude,longitude], secondProjectAnchor?:[easting,northing]}; provide one map coordinate form per anchor; needs a selected CRS; second pair derives scale/rotation")
            .noundo()
            .enabled(has_map_crs)
            .interactive(|s| {
                let crs = s
                    .state()?
                    .map
                    .registration
                    .map(Registration::crs)
                    .ok_or_else(|| bad("map.georef.align", "select the drawing CRS first with MAP.GEOREF.SET"))?;
                Ok(Box::new(AlignM::new(crs)))
            }),
        CommandSpec::new("map.georef.clear", "Clear Map Georeference", georef_clear).params("{}").noundo(),
        CommandSpec::new("map.status", "Map Status", status).params("{}").noundo(),
    ]
}

fn has_map_crs(s: &Session) -> std::result::Result<(), String> {
    if s.state().is_ok_and(|state| state.map.registration.is_some()) {
        Ok(())
    } else {
        Err("select the drawing CRS first with MAP.GEOREF.SET".into())
    }
}

fn mode(s: &mut Session, p: &Value) -> Result<Value> {
    let mode = match p.get("mode").and_then(Value::as_str).map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("off") => MapMode::Off,
        Some("street") => MapMode::Street,
        Some("satellite") => MapMode::Satellite,
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
    let epsg = p.get("crs").and_then(Value::as_str).map(str::trim);
    let crs = match epsg {
        Some("EPSG:4326") => CrsId::Wgs84,
        Some("EPSG:3857") => CrsId::WebMercator,
        Some(value) => CrsId::MGA.iter().copied().find(|crs| crs.epsg() == value).ok_or_else(|| {
            bad("map.georef.set", "`crs` must be EPSG:4326, EPSG:3857, or a supported MGA zone (EPSG:7846-7856 or EPSG:28346-28356)")
        })?,
        None => return Err(bad("map.georef.set", "`crs` must be an EPSG identifier")),
    };
    // CRS-only selection is the normal survey workflow: an identity transform preserves every
    // drawing coordinate as-is. The zero/zero anchors are merely an algebraic identity, not a
    // claim that the drawing's origin is a survey control point.
    let drawing = match (p.get("drawingAnchor"), p.get("projectAnchor")) {
        (None, None) => cadcraft_geom::Vec2::ZERO,
        (Some(_), Some(_)) => point_req("map.georef.set", p, "drawingAnchor")?,
        _ => return Err(bad("map.georef.set", "`drawingAnchor` and `projectAnchor` must be supplied together")),
    };
    let project = match (p.get("drawingAnchor"), p.get("projectAnchor")) {
        (None, None) => cadcraft_geom::Vec2::ZERO,
        (Some(_), Some(_)) => point_req("map.georef.set", p, "projectAnchor")?,
        _ => return Err(bad("map.georef.set", "`drawingAnchor` and `projectAnchor` must be supplied together")),
    };
    let scale = optional_finite(p, "scale", 1.0, true)?;
    let rotation = optional_finite(p, "rotation", 0.0, false)?;
    let drawing = Coordinate::new(drawing.x, drawing.y).map_err(|error| bad("map.georef.set", error.to_string()))?;
    let project = Coordinate::new(project.x, project.y).map_err(|error| bad("map.georef.set", error.to_string()))?;
    let registration = Registration::new(crs, drawing, project, scale, rotation).map_err(|error| bad("map.georef.set", error.to_string()))?;
    s.state_mut()?.map.registration = Some(registration);
    s.touch();
    status(s, &Value::Null)
}

fn georef_align(s: &mut Session, p: &Value) -> Result<Value> {
    let crs = s
        .state()?
        .map
        .registration
        .map(Registration::crs)
        .ok_or_else(|| bad("map.georef.align", "select the drawing CRS first with MAP.GEOREF.SET"))?;
    let drawing = point_req("map.georef.align", p, "drawingAnchor")?;
    let project = map_anchor(p, "projectAnchor", "latLongAnchor", crs)?;
    let second_drawing = p.get("secondDrawingAnchor").map(|_| point_req("map.georef.align", p, "secondDrawingAnchor")).transpose()?;
    let has_second_project = p.get("secondProjectAnchor").is_some();
    let has_second_lat_long = p.get("secondLatLongAnchor").is_some();
    if has_second_project && has_second_lat_long {
        return Err(bad("map.georef.align", "provide only one of `secondProjectAnchor` or `secondLatLongAnchor`"));
    }
    let second_project =
        if has_second_project || has_second_lat_long { Some(map_anchor(p, "secondProjectAnchor", "secondLatLongAnchor", crs)?) } else { None };
    if second_drawing.is_some() != second_project.is_some() {
        return Err(bad("map.georef.align", "`secondDrawingAnchor` and `secondProjectAnchor` must be supplied together"));
    }
    let registration = aligned_registration(crs, drawing, project, second_drawing.zip(second_project))?;
    s.state_mut()?.map.registration = Some(registration);
    s.touch();
    let summary = alignment_summary(registration);
    s.log.push(summary.clone());
    Ok(json!({ "crs": crs.epsg(), "scale": registration.scale(), "rotationDegrees": registration.rotation().to_degrees(), "message": summary }))
}

/// Read either a coordinate in the selected project CRS (x/y) or an explicit
/// [latitude, longitude] pair and convert the latter into the selected CRS.
fn map_anchor(p: &Value, project_key: &str, lat_long_key: &str, crs: CrsId) -> Result<cadcraft_geom::Vec2> {
    match (p.get(project_key), p.get(lat_long_key)) {
        (Some(_), Some(_)) => Err(bad("map.georef.align", format!("provide only one of `{project_key}` or `{lat_long_key}`"))),
        (Some(_), None) => point_req("map.georef.align", p, project_key),
        (None, Some(_)) => {
            let point = point_req("map.georef.align", p, lat_long_key)?;
            let lat_long = cadcraft_map::LonLat::new(point.y, point.x).map_err(|error| {
                bad("map.georef.align", format!("`{lat_long_key}` must be [latitude, longitude] within valid map bounds: {error}"))
            })?;
            let project = cadcraft_map::from_wgs84(crs, lat_long)
                .map_err(|error| bad("map.georef.align", format!("cannot convert `{lat_long_key}` to {}: {error}", crs.epsg())))?;
            Ok(cadcraft_geom::Vec2::new(project.x(), project.y()))
        }
        (None, None) => Err(bad("map.georef.align", format!("one of `{project_key}` or `{lat_long_key}` is required"))),
    }
}

fn aligned_registration(
    crs: CrsId,
    drawing: cadcraft_geom::Vec2,
    project: cadcraft_geom::Vec2,
    second: Option<(cadcraft_geom::Vec2, cadcraft_geom::Vec2)>,
) -> Result<Registration> {
    let mut scale: f64 = 1.0;
    let mut rotation: f64 = 0.0;
    if let Some((drawing2, project2)) = second {
        let drawing_delta = drawing2 - drawing;
        let project_delta = project2 - project;
        let drawing_distance = drawing_delta.len();
        let project_distance = project_delta.len();
        if !(drawing_distance.is_finite() && project_distance.is_finite() && drawing_distance > 1e-9 && project_distance > 1e-9) {
            return Err(bad("map.georef.align", "the two points in each coordinate system must be distinct"));
        }
        scale = project_distance / drawing_distance;
        rotation = project_delta.angle() - drawing_delta.angle();
        if !(scale.is_finite() && scale > 0.0 && rotation.is_finite()) {
            return Err(bad("map.georef.align", "the point pairs do not produce a valid scale and rotation"));
        }
    }
    let drawing = Coordinate::new(drawing.x, drawing.y).map_err(|error| bad("map.georef.align", error.to_string()))?;
    let project = Coordinate::new(project.x, project.y).map_err(|error| bad("map.georef.align", error.to_string()))?;
    cadcraft_map::to_wgs84(crs, project)
        .map_err(|error| bad("map.georef.align", format!("project anchor is invalid for {}: {error}", crs.epsg())))?;
    if let Some((_, project2)) = second {
        let project2 = Coordinate::new(project2.x, project2.y).map_err(|error| bad("map.georef.align", error.to_string()))?;
        cadcraft_map::to_wgs84(crs, project2)
            .map_err(|error| bad("map.georef.align", format!("second project anchor is invalid for {}: {error}", crs.epsg())))?;
    }
    Registration::new(crs, drawing, project, scale, rotation).map_err(|error| bad("map.georef.align", error.to_string()))
}

fn alignment_summary(registration: Registration) -> String {
    format!(
        "Map aligned to drawing using {}: scale {:.8}, rotation {:.6} degrees. Drawing geometry was not changed.",
        registration.crs().epsg(),
        registration.scale(),
        registration.rotation().to_degrees()
    )
}

#[derive(Clone, Copy)]
enum AlignAsk {
    Drawing1,
    Project1,
    Drawing2,
    Project2,
}

struct AlignM {
    crs: CrsId,
    ask: AlignAsk,
    drawing1: Option<cadcraft_geom::Vec2>,
    project1: Option<cadcraft_geom::Vec2>,
    drawing2: Option<cadcraft_geom::Vec2>,
}

impl AlignM {
    fn new(crs: CrsId) -> Self {
        Self { crs, ask: AlignAsk::Drawing1, drawing1: None, project1: None, drawing2: None }
    }

    fn apply(&self, s: &mut Session, second: Option<(cadcraft_geom::Vec2, cadcraft_geom::Vec2)>) -> Result<Step> {
        let Some(drawing) = self.drawing1 else { return Ok(Step::Cancel) };
        let Some(project) = self.project1 else { return Ok(Step::Cancel) };
        let registration = aligned_registration(self.crs, drawing, project, second)?;
        s.state_mut()?.map.registration = Some(registration);
        s.touch();
        s.log.push(alignment_summary(registration));
        Ok(Step::Done)
    }

    fn parse_lat_long(text: &str, crs: CrsId) -> Option<cadcraft_geom::Vec2> {
        let (latitude, longitude) = text.trim().split_once(',')?;
        if longitude.contains(',') {
            return None;
        }
        let latitude = latitude.trim().parse().ok()?;
        let longitude = longitude.trim().parse().ok()?;
        let lat_long = cadcraft_map::LonLat::new(longitude, latitude).ok()?;
        let project = cadcraft_map::from_wgs84(crs, lat_long).ok()?;
        Some(cadcraft_geom::Vec2::new(project.x(), project.y()))
    }
}

impl Interactive for AlignM {
    fn name(&self) -> &'static str {
        "MAP.GEOREF.ALIGN"
    }

    fn prompt(&self, _: &Session) -> Prompt {
        match self.ask {
            AlignAsk::Drawing1 => Prompt::new("Specify drawing anchor point", Accept::POINT),
            AlignAsk::Project1 => Prompt::new("Enter matching map location (latitude,longitude)", Accept::TEXT),
            AlignAsk::Drawing2 => {
                Prompt::new("Specify second drawing anchor for scale/rotation (or press ENTER for translation only)", Accept::POINT)
            }
            AlignAsk::Project2 => Prompt::new("Enter second matching map location (latitude,longitude)", Accept::TEXT),
        }
    }

    fn input(&mut self, s: &mut Session, input: Input) -> Result<Step> {
        match (self.ask, input) {
            (_, Input::Cancel) => return Ok(Step::Cancel),
            (AlignAsk::Drawing1, Input::Point(point)) if point.is_finite() => {
                self.drawing1 = Some(point);
                self.ask = AlignAsk::Project1;
            }
            (AlignAsk::Project1, Input::Text(text)) => {
                self.project1 = Some(
                    Self::parse_lat_long(&text, self.crs)
                        .ok_or_else(|| bad("map.georef.align", "enter a valid latitude,longitude pair, for example -37.8,144.9"))?,
                );
                self.ask = AlignAsk::Drawing2;
            }
            (AlignAsk::Drawing2, Input::Enter) => return self.apply(s, None),
            (AlignAsk::Drawing2, Input::Point(point)) if point.is_finite() => {
                self.drawing2 = Some(point);
                self.ask = AlignAsk::Project2;
            }
            (AlignAsk::Project2, Input::Text(text)) => {
                let project2 = Self::parse_lat_long(&text, self.crs)
                    .ok_or_else(|| bad("map.georef.align", "enter a valid latitude,longitude pair, for example -37.8000,144.9001"))?;
                let Some(drawing2) = self.drawing2 else { return Ok(Step::Cancel) };
                return self.apply(s, Some((drawing2, project2)));
            }
            (_, Input::Enter) => return Ok(Step::Cancel),
            _ => return Ok(Step::Continue),
        }
        Ok(Step::Continue)
    }
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
        "provider": match map.mode {
            MapMode::Street => Some("openstreetmap-standard"),
            MapMode::Satellite => Some("google-maps-satellite"),
            MapMode::Off => None,
        },
        "registered": map.registration.is_some(),
        "crs": map.registration.map(|registration| registration.crs().epsg()),
        "status": if map.mode == MapMode::Off { "off" } else if map.registration.is_none() { "georeference required" } else { "ready" },
    }))
}

#[cfg(test)]
#[allow(clippy::unreachable)]
mod tests {
    use super::*;

    fn coord(x: f64, y: f64) -> Coordinate {
        match Coordinate::new(x, y) {
            Ok(value) => value,
            Err(error) => unreachable!("test coordinate is finite: {error}"),
        }
    }

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
        assert!(session.execute("map.georef.set", &json!({"crs":"EPSG:28350"})).is_ok());
        let status = match session.execute("map.status", &json!({})) {
            Ok(value) => value,
            Err(error) => unreachable!("status must succeed: {error}"),
        };
        assert_eq!(status["crs"], "EPSG:28350");
        let satellite = match session.execute("map.mode", &json!({"mode":"satellite"})) {
            Ok(value) => value,
            Err(error) => unreachable!("satellite mode is a valid provider selection: {error}"),
        };
        assert_eq!(satellite["provider"], "google-maps-satellite");
        let state = match session.state() {
            Ok(state) => state,
            Err(error) => unreachable!("session state exists: {error}"),
        };
        let registration = match state.map.registration {
            Some(registration) => registration,
            None => unreachable!("CRS selection stores the selected CRS"),
        };
        let survey_coordinate = match Coordinate::new(321_000.0, 5_812_000.0) {
            Ok(value) => value,
            Err(error) => unreachable!("test coordinate is finite: {error}"),
        };
        assert_eq!(registration.to_project(survey_coordinate), Ok(survey_coordinate));
        assert!(session.execute("map.georef.set", &json!({"crs":"EPSG:7855", "drawingAnchor":[1,2]})).is_err());
    }

    #[test]
    fn one_point_map_alignment_translates_without_changing_entities() {
        let mut session = Session::new();
        assert!(session.execute("map.georef.set", &json!({"crs":"EPSG:7855"})).is_ok());
        assert!(session.execute("line", &json!({"points":[[1,2],[3,4]]})).is_ok());
        let before = session.doc().map(|doc| doc.entity_count()).unwrap_or(0);
        let result = session.execute("map.georef.align", &json!({"drawingAnchor":[10,20], "projectAnchor":[321000,5812000]}));
        assert!(result.is_ok(), "{result:?}");
        let registration = session.state().ok().and_then(|state| state.map.registration);
        assert!(registration.is_some());
        let Some(registration) = registration else { return };
        assert_eq!(registration.scale(), 1.0);
        assert_eq!(registration.rotation(), 0.0);
        assert_eq!(registration.to_project(coord(10.0, 20.0)), Ok(coord(321000.0, 5812000.0)));
        assert_eq!(session.doc().map(|doc| doc.entity_count()).unwrap_or(0), before);
        assert!(session.log.iter().any(|line| line.contains("geometry was not changed")));
    }

    #[test]
    fn map_state_and_alignment_do_not_change_the_drawing_or_dxf_export() {
        let mut session = Session::new();
        assert!(session.execute("line", &json!({"points":[[10,20],[30,40]]})).is_ok());
        let before = match session.doc() {
            Ok(document) => cadcraft_io::write_dxf(document),
            Err(error) => unreachable!("a new session has a drawing: {error}"),
        };

        assert!(session.execute("map.georef.set", &json!({"crs":"EPSG:7855"})).is_ok());
        assert!(session.execute("map.mode", &json!({"mode":"street"})).is_ok());
        assert!(session.execute("map.opacity", &json!({"opacity":0.4})).is_ok());
        assert!(session.execute("map.georef.align", &json!({"drawingAnchor":[10,20], "latLongAnchor":[-37.8,144.9]})).is_ok());

        let after = match session.doc() {
            Ok(document) => cadcraft_io::write_dxf(document),
            Err(error) => unreachable!("map commands keep the drawing available: {error}"),
        };
        assert_eq!(after, before, "map state must not enter CAD drawing data or its ordinary DXF export");
    }

    #[test]
    fn latitude_longitude_anchor_is_converted_to_selected_crs() {
        let mut session = Session::new();
        assert!(session.execute("map.georef.set", &json!({"crs":"EPSG:7855"})).is_ok());
        let result = session.execute("map.georef.align", &json!({"drawingAnchor":[10,20], "latLongAnchor":[-37.8,144.9]}));
        assert!(result.is_ok(), "{result:?}");
        let registration = session.state().ok().and_then(|state| state.map.registration);
        assert!(registration.is_some());
        let Some(registration) = registration else { return };
        let expected =
            match cadcraft_map::from_wgs84(CrsId::Gda2020MgaZone55, cadcraft_map::LonLat::new(144.9, -37.8).unwrap_or_else(|_| unreachable!())) {
                Ok(value) => value,
                Err(error) => unreachable!("valid Melbourne coordinate transforms: {error}"),
            };
        assert!((registration.project_anchor().x() - expected.x()).abs() < 1e-6);
        assert!((registration.project_anchor().y() - expected.y()).abs() < 1e-6);
        assert!(session.execute("map.georef.align", &json!({"drawingAnchor":[0,0], "latLongAnchor":[91,144.9]})).is_err());
        assert!(session.execute("map.georef.align", &json!({"drawingAnchor":[0,0], "projectAnchor":[1,2], "latLongAnchor":[-37.8,144.9]})).is_err());
    }

    #[test]
    fn two_point_map_alignment_derives_scale_and_rotation_and_is_interactive() {
        let mut session = Session::new();
        assert!(session.execute("map.georef.set", &json!({"crs":"EPSG:7855"})).is_ok());
        assert!(session.start("map.georef.align").is_ok());
        assert!(session.input(Input::Point(cadcraft_geom::Vec2::new(10.0, 20.0))).is_ok());
        assert!(session.input(Input::Text("-37.8,144.9".into())).is_ok());
        assert!(session.input(Input::Point(cadcraft_geom::Vec2::new(20.0, 20.0))).is_ok());
        assert!(session.input(Input::Text("-37.8,144.9001".into())).is_ok());
        let registration = session.state().ok().and_then(|state| state.map.registration);
        assert!(registration.is_some());
        let Some(registration) = registration else { return };
        let first = AlignM::parse_lat_long("-37.8,144.9", CrsId::Gda2020MgaZone55);
        let second = AlignM::parse_lat_long("-37.8,144.9001", CrsId::Gda2020MgaZone55);
        assert!(first.is_some() && second.is_some());
        let (Some(first), Some(second)) = (first, second) else { return };
        let expected_scale = (second - first).len() / 10.0;
        let expected_rotation = (second - first).angle();
        assert!((registration.scale() - expected_scale).abs() < 1e-12);
        assert!((registration.rotation() - expected_rotation).abs() < 1e-12);
        assert_eq!(registration.to_project(coord(20.0, 20.0)), Ok(coord(second.x, second.y)));
    }

    #[test]
    fn map_alignment_refuses_partial_or_coincident_second_pairs() {
        let mut session = Session::new();
        assert!(session.execute("map.georef.set", &json!({"crs":"EPSG:7855"})).is_ok());
        assert!(session.execute("map.georef.align", &json!({"drawingAnchor":[0,0], "projectAnchor":[10,10], "secondDrawingAnchor":[1,0]})).is_err());
        assert!(
            session
                .execute(
                    "map.georef.align",
                    &json!({"drawingAnchor":[0,0], "projectAnchor":[10,10], "secondDrawingAnchor":[0,0], "secondProjectAnchor":[20,20]})
                )
                .is_err()
        );
        assert!(session.execute("map.georef.set", &json!({"crs":"EPSG:4326"})).is_ok());
        assert!(session.execute("map.georef.align", &json!({"drawingAnchor":[0,0], "projectAnchor":[144.9,100]})).is_err());
    }
}
