//! Typed, validated mask geometry for the supported non-AI mask kinds.
//!
//! # Coordinate contract (lap-78d / rapidraw-922)
//!
//! Mask geometry shares the documented oriented frame with [`crate::geometry`]
//! (origin at the top-left of the source image after `orientation_steps`
//! clockwise quarter turns and the flip flags):
//!
//! - **Positions** (points, centers, gradient start/end) are normalized
//!   fractions of the oriented full-frame size: `x` in units of the oriented
//!   width, `y` in units of the oriented height. Values may leave the frame
//!   (strokes/gradients can extend past the edges) but are bounded by
//!   [`MAX_GEOMETRY_COORD`].
//! - **Lengths** (brush diameter, ellipse radii, gradient range) are
//!   normalized fractions of the oriented full-frame **width**. A circle
//!   stays a circle for any frame aspect ratio because rasterization
//!   converts lengths and positions back to oriented pixels before applying
//!   the reference per-pixel formulas.
//! - `rotation` is degrees in `[0, 360)`, `feather` is `[0, 1]`, `flow` is
//!   `[0, 100]`.
//!
//! # Legacy payload bridge
//!
//! The legacy RapidRAW `.rrdata` payload stores the same quantities in
//! oriented **pixels** (`parameters` blob, opaque in the recipe schema).
//! [`MaskGeometry::from_legacy`] converts a supported legacy payload into the
//! typed form; [`MaskGeometry::to_legacy`] regenerates the pixel payload.
//! Unsupported kinds (AI segmentation/depth, luminance/color range masks)
//! convert to `Ok(None)`: they are never silently dropped, they simply have
//! no typed representation and remain preserved verbatim in `SubMask::
//! parameters` with explicit limitation reporting by the host.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ModelError;

/// Maximum number of stroke lines per brush/flow geometry.
pub const MAX_GEOMETRY_LINES: usize = 256;
/// Maximum number of points per stroke line.
pub const MAX_GEOMETRY_POINTS_PER_LINE: usize = 4096;
/// Maximum total points per sub-mask geometry (memory bound).
pub const MAX_GEOMETRY_TOTAL_POINTS: usize = 16_384;
/// Inclusive bound for normalized positions and lengths. Generous enough for
/// off-frame work at real frame sizes while rejecting sentinel payloads
/// (e.g. the legacy `-10000` initial-draw placeholders).
pub const MAX_GEOMETRY_COORD: f64 = 16.0;

/// Stroke tool of a brush/flow line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BrushTool {
    #[serde(rename = "brush")]
    #[default]
    Brush,
    #[serde(rename = "eraser")]
    Eraser,
}

/// One stroke point, normalized to the oriented frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskPoint {
    pub x: f64,
    pub y: f64,
}

/// One brush stroke line. `brush_size` is a diameter normalized to the
/// oriented frame width, exactly like the reference `brushSize` pixel field
/// (a diameter, halved to a radius at rasterization).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrushLine {
    pub tool: BrushTool,
    pub brush_size: f64,
    /// Smoothstep feather fraction in [0, 1] (0 = hard edge).
    pub feather: f64,
    pub points: Vec<MaskPoint>,
}

/// One flow stroke line: brush geometry with per-stroke accumulation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowLine {
    pub tool: BrushTool,
    pub brush_size: f64,
    pub feather: f64,
    /// Per-stroke accumulation in [0, 100].
    pub flow: f64,
    pub points: Vec<MaskPoint>,
}

/// Typed geometry of one supported sub-mask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum MaskGeometry {
    Brush {
        lines: Vec<BrushLine>,
    },
    Flow {
        lines: Vec<FlowLine>,
    },
    Linear {
        start_x: f64,
        start_y: f64,
        end_x: f64,
        end_y: f64,
        /// Falloff half-width normalized to the oriented frame width.
        range: f64,
    },
    Radial {
        center_x: f64,
        center_y: f64,
        radius_x: f64,
        radius_y: f64,
        /// Degrees in [0, 360).
        rotation: f64,
        feather: f64,
    },
    /// The whole frame at full influence.
    All,
}

/// Mask kinds the typed geometry schema supports.
pub const SUPPORTED_KINDS: [&str; 5] = ["brush", "flow", "linear", "radial", "all"];

/// Whether `kind` has a typed geometry representation.
pub fn is_supported_kind(kind: &str) -> bool {
    SUPPORTED_KINDS.contains(&kind)
}

fn check_finite(path: &str, value: f64) -> Result<(), ModelError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(ModelError::Validation(format!(
            "mask geometry {path} is not finite"
        )))
    }
}

fn check_bound(path: &str, value: f64, min: f64, max: f64) -> Result<(), ModelError> {
    check_finite(path, value)?;
    if value < min || value > max {
        return Err(ModelError::Validation(format!(
            "mask geometry {path} {value} outside [{min}, {max}]"
        )));
    }
    Ok(())
}

fn check_position(path: &str, x: f64, y: f64) -> Result<(), ModelError> {
    check_bound(
        &format!("{path}.x"),
        x,
        -MAX_GEOMETRY_COORD,
        MAX_GEOMETRY_COORD,
    )?;
    check_bound(
        &format!("{path}.y"),
        y,
        -MAX_GEOMETRY_COORD,
        MAX_GEOMETRY_COORD,
    )
}

fn check_points(path: &str, points: &[MaskPoint]) -> Result<(), ModelError> {
    if points.len() > MAX_GEOMETRY_POINTS_PER_LINE {
        return Err(ModelError::Validation(format!(
            "mask geometry {path} has {} points, exceeding {MAX_GEOMETRY_POINTS_PER_LINE}",
            points.len()
        )));
    }
    for (index, point) in points.iter().enumerate() {
        check_position(&format!("{path}.points[{index}]"), point.x, point.y)?;
    }
    Ok(())
}

impl MaskGeometry {
    /// Converts a legacy pixel `parameters` payload of `kind` into typed
    /// geometry. `Ok(None)` marks an unsupported kind (payload stays
    /// preserved opaquely); `Err` marks a malformed supported payload.
    /// `oriented_width`/`oriented_height` are the oriented full-frame
    /// dimensions the legacy pixels refer to.
    pub fn from_legacy(
        kind: &str,
        parameters: &Value,
        oriented_width: f64,
        oriented_height: f64,
    ) -> Result<Option<MaskGeometry>, ModelError> {
        if !oriented_width.is_finite() || oriented_width <= 0.0 {
            return Err(ModelError::Validation(
                "mask geometry conversion needs a positive oriented width".to_string(),
            ));
        }
        if !oriented_height.is_finite() || oriented_height <= 0.0 {
            return Err(ModelError::Validation(
                "mask geometry conversion needs a positive oriented height".to_string(),
            ));
        }
        if !is_supported_kind(kind) {
            return Ok(None);
        }
        let px = |v: Value| -> Result<f64, ModelError> {
            v.as_f64().ok_or_else(|| {
                ModelError::Validation(format!(
                    "mask geometry field for kind {kind} is missing or not numeric"
                ))
            })
        };
        let geometry = match kind {
            "brush" => {
                let lines = legacy_lines(parameters, oriented_width, oriented_height)?;
                MaskGeometry::Brush { lines }
            }
            "flow" => {
                let lines = legacy_flow_lines(parameters, oriented_width, oriented_height)?;
                MaskGeometry::Flow { lines }
            }
            "linear" => {
                let obj = parameters.as_object().ok_or_else(|| {
                    ModelError::Validation(
                        "mask geometry linear payload must be an object".to_string(),
                    )
                })?;
                let get = |key: &str| px(obj.get(key).cloned().unwrap_or(Value::Null));
                MaskGeometry::Linear {
                    start_x: get("startX")? / oriented_width,
                    start_y: get("startY")? / oriented_height,
                    end_x: get("endX")? / oriented_width,
                    end_y: get("endY")? / oriented_height,
                    range: get("range")? / oriented_width,
                }
            }
            "radial" => {
                let obj = parameters.as_object().ok_or_else(|| {
                    ModelError::Validation(
                        "mask geometry radial payload must be an object".to_string(),
                    )
                })?;
                let get = |key: &str| px(obj.get(key).cloned().unwrap_or(Value::Null));
                MaskGeometry::Radial {
                    center_x: get("centerX")? / oriented_width,
                    center_y: get("centerY")? / oriented_height,
                    radius_x: get("radiusX")? / oriented_width,
                    radius_y: get("radiusY")? / oriented_width,
                    rotation: get("rotation")?,
                    feather: get("feather")?,
                }
            }
            "all" => MaskGeometry::All,
            other => {
                return Err(ModelError::Validation(format!(
                    "mask kind {other} is not a supported typed geometry"
                )));
            }
        };
        geometry.validate()?;
        Ok(Some(geometry))
    }

    /// Regenerates the legacy pixel payload (`kind`, `parameters`) for this
    /// geometry, inverting [`MaskGeometry::from_legacy`].
    pub fn to_legacy(
        &self,
        oriented_width: f64,
        oriented_height: f64,
    ) -> Result<(String, Value), ModelError> {
        if !oriented_width.is_finite() || oriented_width <= 0.0 {
            return Err(ModelError::Validation(
                "mask geometry conversion needs a positive oriented width".to_string(),
            ));
        }
        if !oriented_height.is_finite() || oriented_height <= 0.0 {
            return Err(ModelError::Validation(
                "mask geometry conversion needs a positive oriented height".to_string(),
            ));
        }
        match self {
            MaskGeometry::Brush { lines } => {
                let payload = json!({ "lines": lines.iter().map(|line| json!({
                    "tool": line.tool,
                    "brushSize": line.brush_size * oriented_width,
                    "feather": line.feather,
                    "points": line.points.iter().map(|p| json!({
                        "x": p.x * oriented_width, "y": p.y * oriented_height
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>() });
                Ok(("brush".to_string(), payload))
            }
            MaskGeometry::Flow { lines } => {
                let payload = json!({ "lines": lines.iter().map(|line| json!({
                    "tool": line.tool,
                    "brushSize": line.brush_size * oriented_width,
                    "feather": line.feather,
                    "flow": line.flow,
                    "points": line.points.iter().map(|p| json!({
                        "x": p.x * oriented_width, "y": p.y * oriented_height
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>() });
                Ok(("flow".to_string(), payload))
            }
            MaskGeometry::Linear {
                start_x,
                start_y,
                end_x,
                end_y,
                range,
            } => {
                let payload = json!({
                    "startX": start_x * oriented_width,
                    "startY": start_y * oriented_height,
                    "endX": end_x * oriented_width,
                    "endY": end_y * oriented_height,
                    "range": range * oriented_width,
                });
                Ok(("linear".to_string(), payload))
            }
            MaskGeometry::Radial {
                center_x,
                center_y,
                radius_x,
                radius_y,
                rotation,
                feather,
            } => {
                let payload = json!({
                    "centerX": center_x * oriented_width,
                    "centerY": center_y * oriented_height,
                    "radiusX": radius_x * oriented_width,
                    "radiusY": radius_y * oriented_width,
                    "rotation": rotation,
                    "feather": feather,
                });
                Ok(("radial".to_string(), payload))
            }
            MaskGeometry::All => Ok(("all".to_string(), json!({}))),
        }
    }

    /// Enforces the documented bounds (also applied by recipe validation).
    pub fn validate(&self) -> Result<(), ModelError> {
        match self {
            MaskGeometry::Brush { lines } => {
                check_line_count(lines.len())?;
                let mut total = 0usize;
                for (index, line) in lines.iter().enumerate() {
                    total += check_line(&format!("lines[{index}]"), line)?;
                }
                check_total_points(total)?;
            }
            MaskGeometry::Flow { lines } => {
                check_line_count(lines.len())?;
                let mut total = 0usize;
                for (index, line) in lines.iter().enumerate() {
                    total += check_flow_line(&format!("lines[{index}]"), line)?;
                }
                check_total_points(total)?;
            }
            MaskGeometry::Linear {
                start_x,
                start_y,
                end_x,
                end_y,
                range,
            } => {
                check_position("start", *start_x, *start_y)?;
                check_position("end", *end_x, *end_y)?;
                check_bound("range", *range, f64::MIN_POSITIVE, MAX_GEOMETRY_COORD)?;
            }
            MaskGeometry::Radial {
                center_x,
                center_y,
                radius_x,
                radius_y,
                rotation,
                feather,
            } => {
                check_position("center", *center_x, *center_y)?;
                check_bound("radiusX", *radius_x, f64::MIN_POSITIVE, MAX_GEOMETRY_COORD)?;
                check_bound("radiusY", *radius_y, f64::MIN_POSITIVE, MAX_GEOMETRY_COORD)?;
                check_finite("rotation", *rotation)?;
                check_bound("feather", *feather, 0.0, 1.0)?;
            }
            MaskGeometry::All => {}
        }
        Ok(())
    }
}

fn check_line_count(count: usize) -> Result<(), ModelError> {
    if count > MAX_GEOMETRY_LINES {
        return Err(ModelError::Validation(format!(
            "mask geometry has {count} lines, exceeding {MAX_GEOMETRY_LINES}"
        )));
    }
    Ok(())
}

fn check_total_points(total: usize) -> Result<(), ModelError> {
    if total > MAX_GEOMETRY_TOTAL_POINTS {
        return Err(ModelError::Validation(format!(
            "mask geometry has {total} total points, exceeding {MAX_GEOMETRY_TOTAL_POINTS}"
        )));
    }
    Ok(())
}

fn check_line(path: &str, line: &BrushLine) -> Result<usize, ModelError> {
    check_bound(
        &format!("{path}.brushSize"),
        line.brush_size,
        f64::MIN_POSITIVE,
        MAX_GEOMETRY_COORD,
    )?;
    check_bound(&format!("{path}.feather"), line.feather, 0.0, 1.0)?;
    check_points(path, &line.points)?;
    Ok(line.points.len())
}

fn check_flow_line(path: &str, line: &FlowLine) -> Result<usize, ModelError> {
    check_bound(
        &format!("{path}.brushSize"),
        line.brush_size,
        f64::MIN_POSITIVE,
        MAX_GEOMETRY_COORD,
    )?;
    check_bound(&format!("{path}.feather"), line.feather, 0.0, 1.0)?;
    check_bound(&format!("{path}.flow"), line.flow, 0.0, 100.0)?;
    check_points(path, &line.points)?;
    Ok(line.points.len())
}

/// Legacy brush-line payload shape (oriented pixels), mirroring the
/// reference serde struct including its defaults (`feather` 0.5).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyBrushLine {
    #[serde(default)]
    tool: String,
    brush_size: f64,
    #[serde(default = "default_legacy_feather")]
    feather: f64,
    #[serde(default)]
    points: Vec<LegacyPoint>,
}

#[derive(Deserialize)]
struct LegacyPoint {
    x: f64,
    y: f64,
}

fn default_legacy_feather() -> f64 {
    0.5
}

/// Legacy flow-line payload shape (`flow` default 10 like the reference).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyFlowLine {
    #[serde(default)]
    tool: String,
    brush_size: f64,
    #[serde(default = "default_legacy_feather")]
    feather: f64,
    #[serde(default = "default_legacy_flow")]
    flow: f64,
    #[serde(default)]
    points: Vec<LegacyPoint>,
}

fn default_legacy_flow() -> f64 {
    10.0
}

fn legacy_lines(
    parameters: &Value,
    oriented_width: f64,
    oriented_height: f64,
) -> Result<Vec<BrushLine>, ModelError> {
    let lines: Vec<LegacyBrushLine> =
        serde_json::from_value(parameters.get("lines").cloned().ok_or_else(|| {
            ModelError::Validation("mask geometry brush payload has no lines".to_string())
        })?)
        .map_err(|err| {
            ModelError::Validation(format!("mask geometry brush lines are malformed: {err}"))
        })?;
    let converted = lines
        .into_iter()
        .map(|line| BrushLine {
            tool: if line.tool == "eraser" {
                BrushTool::Eraser
            } else {
                BrushTool::Brush
            },
            brush_size: line.brush_size / oriented_width,
            feather: line.feather,
            points: convert_points(line.points, oriented_width, oriented_height),
        })
        .collect();
    Ok(converted)
}

fn legacy_flow_lines(
    parameters: &Value,
    oriented_width: f64,
    oriented_height: f64,
) -> Result<Vec<FlowLine>, ModelError> {
    let lines: Vec<LegacyFlowLine> =
        serde_json::from_value(parameters.get("lines").cloned().ok_or_else(|| {
            ModelError::Validation("mask geometry flow payload has no lines".to_string())
        })?)
        .map_err(|err| {
            ModelError::Validation(format!("mask geometry flow lines are malformed: {err}"))
        })?;
    let converted = lines
        .into_iter()
        .map(|line| FlowLine {
            tool: if line.tool == "eraser" {
                BrushTool::Eraser
            } else {
                BrushTool::Brush
            },
            brush_size: line.brush_size / oriented_width,
            feather: line.feather,
            flow: line.flow,
            points: convert_points(line.points, oriented_width, oriented_height),
        })
        .collect();
    Ok(converted)
}

fn convert_points(
    points: Vec<LegacyPoint>,
    oriented_width: f64,
    oriented_height: f64,
) -> Vec<MaskPoint> {
    points
        .into_iter()
        .map(|point| MaskPoint {
            x: point.x / oriented_width,
            y: point.y / oriented_height,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_kind_list_matches_the_tagged_enum() {
        for kind in SUPPORTED_KINDS {
            assert!(is_supported_kind(kind));
        }
        assert!(!is_supported_kind("ai-subject"));
        assert!(!is_supported_kind(""));
    }

    #[test]
    fn total_point_budget_bounds_many_short_lines() {
        let lines: Vec<Value> = (0..MAX_GEOMETRY_LINES)
            .map(|_| {
                json!({
                    "tool": "brush", "brushSize": 10.0, "feather": 0.5,
                    "points": (0..MAX_GEOMETRY_POINTS_PER_LINE / 2)
                        .map(|i| json!({ "x": i as f64, "y": 0.0 }))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let payload = json!({ "lines": lines });
        let error = MaskGeometry::from_legacy("brush", &payload, 2000.0, 1000.0)
            .expect_err("total point budget must fail");
        assert!(error.to_string().contains("total points"));
    }
}
