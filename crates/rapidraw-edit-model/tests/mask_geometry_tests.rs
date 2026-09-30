//! Behavior tests for the typed, validated mask geometry schema (lap-78d /
//! rapidraw-922).
//!
//! Coverage: legacy pixel payload conversion into the documented oriented
//! normalized coordinates, round trips, unsupported-kind preservation,
//! explicit malformed-payload errors, bounded limits, serialization of the
//! `SubMask.geometry` field, and recipe-level validation of geometry limits.
//!
//! Coordinate contract under test: positions are normalized fractions of the
//! oriented full-frame size; lengths (brush diameter, radii, gradient range)
//! are normalized fractions of the oriented frame width. Rasterization
//! converts back with (oriented_width, oriented_height) and applies the
//! reference per-pixel formulas in oriented pixels.

use rapidraw_edit_model::masks::{
    MAX_GEOMETRY_COORD, MAX_GEOMETRY_LINES, MAX_GEOMETRY_POINTS_PER_LINE, MaskGeometry,
};
use rapidraw_edit_model::types::{MaskContainer, Recipe, SubMask};
use rapidraw_edit_model::{ModelError, validate_recipe};
use serde_json::json;

const W: f64 = 2000.0;
const H: f64 = 1000.0;

#[test]
fn radial_legacy_pixels_convert_to_normalized_oriented_coordinates() {
    let legacy = json!({
        "centerX": 1000.0, "centerY": 250.0,
        "radiusX": 400.0, "radiusY": 200.0,
        "rotation": 30.0, "feather": 0.25
    });
    let geometry = MaskGeometry::from_legacy("radial", &legacy, W, H)
        .expect("radial payload parses")
        .expect("radial is a supported kind");
    match geometry {
        MaskGeometry::Radial {
            center_x,
            center_y,
            radius_x,
            radius_y,
            rotation,
            feather,
        } => {
            assert_eq!(center_x, 0.5);
            assert_eq!(center_y, 0.25);
            assert_eq!(radius_x, 0.2);
            // Lengths normalize to the oriented WIDTH: 200 px / 2000 px.
            assert_eq!(radius_y, 0.1);
            assert_eq!(rotation, 30.0);
            assert_eq!(feather, 0.25);
        }
        other => panic!("expected radial geometry, got {other:?}"),
    }
}

#[test]
fn brush_legacy_payload_converts_lines_and_points() {
    let legacy = json!({
        "lines": [
            { "tool": "brush", "brushSize": 100.0, "feather": 0.5,
              "points": [ { "x": 200.0, "y": 100.0 }, { "x": 400.0, "y": 300.0 } ] },
            { "tool": "eraser", "brushSize": 50.0, "feather": 0.0,
              "points": [ { "x": 0.0, "y": 0.0 } ] }
        ]
    });
    let geometry = MaskGeometry::from_legacy("brush", &legacy, W, H)
        .expect("brush payload parses")
        .expect("brush is a supported kind");
    let MaskGeometry::Brush { lines } = geometry else {
        panic!("expected brush geometry")
    };
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].brush_size, 100.0 / W);
    assert_eq!(lines[0].points[1].x, 400.0 / W);
    assert_eq!(lines[0].points[1].y, 300.0 / H);
    assert_eq!(lines[1].points[0].x, 0.0);
}

#[test]
fn linear_and_flow_and_all_kinds_convert() {
    let linear =
        json!({ "startX": 0.0, "startY": 500.0, "endX": 2000.0, "endY": 500.0, "range": 100.0 });
    let MaskGeometry::Linear {
        start_x,
        start_y,
        end_x,
        end_y,
        range,
    } = MaskGeometry::from_legacy("linear", &linear, W, H)
        .unwrap()
        .unwrap()
    else {
        panic!("expected linear geometry")
    };
    assert_eq!(
        (start_x, start_y, end_x, end_y, range),
        (0.0, 0.5, 1.0, 0.5, 100.0 / W)
    );

    let flow = json!({
        "flow": 10.0,
        "lines": [ { "tool": "brush", "brushSize": 60.0, "feather": 0.4, "flow": 25.0,
                     "points": [ { "x": 1000.0, "y": 500.0 } ] } ]
    });
    let MaskGeometry::Flow { lines } = MaskGeometry::from_legacy("flow", &flow, W, H)
        .unwrap()
        .unwrap()
    else {
        panic!("expected flow geometry")
    };
    assert_eq!(lines[0].flow, 25.0);
    assert_eq!(lines[0].brush_size, 60.0 / W);

    let all = MaskGeometry::from_legacy("all", &json!({}), W, H)
        .unwrap()
        .unwrap();
    assert_eq!(all, MaskGeometry::All);
}

#[test]
fn unsupported_kinds_return_none_not_an_error() {
    for kind in [
        "ai-subject",
        "ai-sky",
        "ai-foreground",
        "ai-depth",
        "luminance",
        "color",
        "quick-eraser",
    ] {
        let parsed = MaskGeometry::from_legacy(kind, &json!({ "maskDataBase64": null }), W, H)
            .expect("unsupported kinds never fail parsing");
        assert!(parsed.is_none(), "kind {kind} must map to None");
    }
}

#[test]
fn malformed_supported_payloads_are_explicit_errors() {
    let missing = json!({ "centerX": 10.0 });
    assert!(MaskGeometry::from_legacy("radial", &missing, W, H).is_err());
    let nonfinite =
        json!({ "startX": 0.0, "startY": 0.0, "endX": "nope", "endY": 1.0, "range": 5.0 });
    assert!(MaskGeometry::from_legacy("linear", &nonfinite, W, H).is_err());
    let bad_lines = json!({ "lines": "not-an-array" });
    assert!(MaskGeometry::from_legacy("brush", &bad_lines, W, H).is_err());
    assert!(MaskGeometry::from_legacy("radial", &json!(null), W, H).is_err());
}

#[test]
fn legacy_round_trip_preserves_geometry_within_tolerance() {
    let radial = json!({
        "centerX": 640.0, "centerY": 480.0,
        "radiusX": 320.0, "radiusY": 160.0,
        "rotation": 15.0, "feather": 0.3
    });
    let geometry = MaskGeometry::from_legacy("radial", &radial, W, H)
        .unwrap()
        .unwrap();
    let (kind, payload) = geometry
        .to_legacy(W, H)
        .expect("legacy payload regenerates");
    assert_eq!(kind, "radial");
    let reparsed = MaskGeometry::from_legacy("radial", &payload, W, H)
        .unwrap()
        .unwrap();
    assert_eq!(geometry, reparsed);
}

#[test]
fn geometry_limits_are_enforced_on_conversion() {
    // Too many lines.
    let lines: Vec<_> = (0..=MAX_GEOMETRY_LINES)
        .map(|_| json!({ "tool": "brush", "brushSize": 10.0, "feather": 0.5, "points": [ { "x": 1.0, "y": 1.0 } ] }))
        .collect();
    let payload = json!({ "lines": lines });
    let error = MaskGeometry::from_legacy("brush", &payload, W, H)
        .expect_err("line-count limit must fail conversion");
    assert!(error.to_string().contains("lines"));

    // Too many points on one line.
    let points: Vec<_> = (0..=MAX_GEOMETRY_POINTS_PER_LINE)
        .map(|i| json!({ "x": (i as f64) / W, "y": 0.5 }))
        .collect();
    let payload = json!({ "lines": [ { "tool": "brush", "brushSize": 10.0, "feather": 0.5, "points": points } ] });
    assert!(MaskGeometry::from_legacy("brush", &payload, W, H).is_err());

    // Out-of-frame coordinates beyond the documented bound.
    let far = json!({ "centerX": (MAX_GEOMETRY_COORD + 1.0) * W, "centerY": 0.0, "radiusX": 1.0, "radiusY": 1.0, "rotation": 0.0, "feather": 0.5 });
    assert!(MaskGeometry::from_legacy("radial", &far, W, H).is_err());

    // Non-positive lengths.
    let zero_radius = json!({ "centerX": 0.5, "centerY": 0.5, "radiusX": 0.0, "radiusY": 1.0, "rotation": 0.0, "feather": 0.5 });
    assert!(MaskGeometry::from_legacy("radial", &zero_radius, W, H).is_err());

    // Feather outside [0, 1].
    let bad_feather = json!({ "centerX": 0.5, "centerY": 0.5, "radiusX": 1.0, "radiusY": 1.0, "rotation": 0.0, "feather": 1.5 });
    assert!(MaskGeometry::from_legacy("radial", &bad_feather, W, H).is_err());
}

#[test]
fn sub_mask_geometry_field_round_trips_through_serialization() {
    let geometry = MaskGeometry::from_legacy(
        "linear",
        &json!({ "startX": 0.0, "startY": 0.0, "endX": 0.0, "endY": 1000.0, "range": 50.0 }),
        W,
        H,
    )
    .unwrap()
    .unwrap();
    let sub_mask = SubMask {
        id: "sub-1".to_string(),
        name: Some("gradient".to_string()),
        kind: "linear".to_string(),
        geometry: Some(geometry.clone()),
        ..SubMask::default()
    };
    let value = serde_json::to_value(&sub_mask).expect("sub-mask serializes");
    assert_eq!(value["geometry"]["type"], "linear");
    assert_eq!(value["geometry"]["range"], 50.0 / W);
    let parsed: SubMask = serde_json::from_value(value).expect("sub-mask deserializes");
    assert_eq!(parsed.geometry, Some(geometry));

    // Old envelopes without the field keep parsing (schema back-compat):
    // pre-mask-geometry payloads carry the other fields but no `geometry`.
    let legacy_only: SubMask = serde_json::from_value(json!({
        "id": "sub-2", "type": "brush", "parameters": {},
        "invert": false, "visible": true, "opacity": 100.0, "mode": "additive"
    }))
    .expect("legacy sub-mask without geometry parses");
    assert_eq!(legacy_only.geometry, None);
}

#[test]
fn recipe_validation_enforces_geometry_limits() {
    let mut recipe = Recipe::default();
    let oversized = MaskGeometry::Radial {
        center_x: 0.5,
        center_y: 0.5,
        radius_x: MAX_GEOMETRY_COORD + 0.5,
        radius_y: 0.1,
        rotation: 0.0,
        feather: 0.5,
    };
    recipe.masks.push(MaskContainer {
        id: "mask-1".to_string(),
        name: "radial".to_string(),
        sub_masks: vec![SubMask {
            id: "sub-1".to_string(),
            kind: "radial".to_string(),
            geometry: Some(oversized),
            ..SubMask::default()
        }],
        ..MaskContainer::default()
    });
    let error = validate_recipe(&recipe).expect_err("oversized geometry must fail validation");
    assert!(matches!(error, ModelError::Validation(_)));
    assert!(error.to_string().contains("masks[0]"));

    let mut ok = Recipe::default();
    ok.masks.push(MaskContainer {
        id: "mask-1".to_string(),
        name: "ok".to_string(),
        sub_masks: vec![SubMask {
            id: "sub-1".to_string(),
            kind: "radial".to_string(),
            geometry: Some(MaskGeometry::Radial {
                center_x: 0.5,
                center_y: 0.5,
                radius_x: 0.25,
                radius_y: 0.25,
                rotation: 0.0,
                feather: 0.5,
            }),
            ..SubMask::default()
        }],
        ..MaskContainer::default()
    });
    validate_recipe(&ok).expect("bounded geometry validates");
}
