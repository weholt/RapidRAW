//! Host-neutral rasterization of supported non-AI masks (lap-78d /
//! rapidraw-922).
//!
//! Reproduces the pinned RapidRAW `mask_generation.rs` semantics exactly
//! (source revision `5e30bcbb246395d391ba2e9662510641ffe68e6b`) for the
//! geometry kinds this engine supports: `brush`, `flow`, `linear`, `radial`
//! and `all`. AI-dependent kinds (`ai-*`, `quick-eraser`) and image-sampling
//! kinds (`luminance`, `color`) have no typed geometry and fail explicitly
//! with [`MaskRasterError::Unsupported`] naming the mask, sub-mask and kind —
//! they are never silently dropped. This crate carries no AI model or
//! runtime dependency (spec A11).
//!
//! # Coordinate mapping
//!
//! Recipes store geometry in the oriented normalized frame (see
//! `rapidraw_edit_model::masks`). Rasterization converts to oriented pixels
//! and maps into the processing buffer exactly like the reference:
//! `dst = src_px * scale - crop_offset_px * scale` with the uniform scale
//! `out_width / oriented_width`.
//!
//! # Compositing order (reference `generate_mask_bitmap`)
//!
//! Per visible mask container, starting from a black buffer: each visible
//! sub-mask in list order is rasterized, inverted (`255 - p`) if requested,
//! scaled by its opacity (truncating u8 multiply), then combined with the
//! running buffer by mode (`additive` = max, `subtractive` =
//! saturating_sub, `intersect` = min). Finally the container-level invert
//! and opacity are applied. A visible container with zero sub-masks yields
//! a black layer so layer indexes stay aligned with
//! `get_all_adjustments_from_json`'s visible-container order.
//!
//! # Cache
//!
//! [`BoundedMaskCache`] keeps at most 50 entries and is cleared wholesale
//! when the bound would be exceeded, matching the reference's memory-bound
//! behavior. The key covers geometry content, output size, scale and crop
//! offset.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use image::{GrayImage, Luma};
use rapidraw_edit_model::masks::{BrushTool, MaskGeometry};
use rapidraw_edit_model::{CropRect, MaskContainer, SubMask, SubMaskMode};

/// Maximum cached mask bitmaps, matching the reference cache bound.
pub const MASK_CACHE_MAX_ENTRIES: usize = 50;

/// Processing-frame parameters shared by preview and export rasterization.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaskRasterFrame {
    pub oriented_width: f64,
    pub oriented_height: f64,
    /// Crop offset in oriented full-frame pixels.
    pub crop_offset_x: f64,
    pub crop_offset_y: f64,
    pub out_width: u32,
    pub out_height: u32,
}

impl MaskRasterFrame {
    /// Builds the frame from the oriented full-frame dimensions, the recipe
    /// crop (normalized oriented coordinates) and the processing output
    /// size.
    pub fn new(
        oriented_width: u32,
        oriented_height: u32,
        crop: Option<CropRect>,
        out_width: u32,
        out_height: u32,
    ) -> Result<Self, MaskRasterError> {
        if oriented_width == 0 || oriented_height == 0 {
            return Err(MaskRasterError::InvalidFrame(
                "oriented frame dimensions must be positive".to_string(),
            ));
        }
        if out_width == 0 || out_height == 0 {
            return Err(MaskRasterError::InvalidFrame(
                "output dimensions must be positive".to_string(),
            ));
        }
        let (crop_offset_x, crop_offset_y) = match crop {
            Some(crop) => {
                let (w, h) = (f64::from(oriented_width), f64::from(oriented_height));
                if !crop.x.is_finite()
                    || !crop.y.is_finite()
                    || !crop.width.is_finite()
                    || !crop.height.is_finite()
                    || !(crop.width > 0.0 && crop.height > 0.0)
                    || !(0.0..=1.0).contains(&crop.x)
                    || !(0.0..=1.0).contains(&crop.y)
                    || crop.x + crop.width > 1.0
                    || crop.y + crop.height > 1.0
                {
                    return Err(MaskRasterError::InvalidFrame(format!(
                        "crop rectangle {crop:?} exceeds the oriented [0,1] frame"
                    )));
                }
                (crop.x * w, crop.y * h)
            }
            None => (0.0, 0.0),
        };
        let frame = Self {
            oriented_width: f64::from(oriented_width),
            oriented_height: f64::from(oriented_height),
            crop_offset_x,
            crop_offset_y,
            out_width,
            out_height,
        };
        Ok(frame)
    }

    /// Uniform processing scale (output width over oriented full width).
    pub fn scale(&self) -> f32 {
        self.out_width as f32 / self.oriented_width as f32
    }

    /// Crop offset as f32 oriented pixels (the reference passes f32).
    pub fn crop_offset(&self) -> (f32, f32) {
        (self.crop_offset_x as f32, self.crop_offset_y as f32)
    }
}

/// Explicit rasterization failure. Unsupported kinds and unconvertible
/// geometries never silently produce an empty mask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaskRasterError {
    Unsupported {
        mask_id: String,
        sub_mask_id: String,
        kind: String,
        detail: String,
    },
    InvalidGeometry {
        mask_id: String,
        sub_mask_id: String,
        detail: String,
    },
    InvalidFrame(String),
}

impl std::fmt::Display for MaskRasterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MaskRasterError::Unsupported {
                mask_id,
                sub_mask_id,
                kind,
                detail,
            } => write!(
                f,
                "unsupported mask kind '{kind}' (mask {mask_id}, sub-mask {sub_mask_id}): {detail}"
            ),
            MaskRasterError::InvalidGeometry {
                mask_id,
                sub_mask_id,
                detail,
            } => write!(
                f,
                "invalid mask geometry (mask {mask_id}, sub-mask {sub_mask_id}): {detail}"
            ),
            MaskRasterError::InvalidFrame(detail) => {
                write!(f, "invalid mask raster frame: {detail}")
            }
        }
    }
}

impl std::error::Error for MaskRasterError {}

/// Number of visible mask containers (the layer count the GPU path expects).
pub fn visible_mask_count(masks: &[MaskContainer]) -> usize {
    masks.iter().filter(|mask| mask.visible).count()
}

/// Rejects any visible sub-mask this engine cannot rasterize, naming it
/// explicitly. Invisible sub-masks do not affect the render and are not
/// required to have typed geometry (they must be reported by import
/// limitations instead).
pub fn validate_masks_supported(masks: &[MaskContainer]) -> Result<(), MaskRasterError> {
    for mask in masks.iter().filter(|mask| mask.visible) {
        for sub_mask in mask.sub_masks.iter().filter(|s| s.visible) {
            if !rapidraw_edit_model::masks::is_supported_kind(&sub_mask.kind) {
                return Err(MaskRasterError::Unsupported {
                    mask_id: mask.id.clone(),
                    sub_mask_id: sub_mask.id.clone(),
                    kind: sub_mask.kind.clone(),
                    detail: "this engine slice rasterizes brush/flow/linear/radial/all masks only; the mask is preserved in the recipe and never silently dropped".to_string(),
                });
            }
            if sub_mask.geometry.is_none() {
                return Err(MaskRasterError::Unsupported {
                    mask_id: mask.id.clone(),
                    sub_mask_id: sub_mask.id.clone(),
                    kind: sub_mask.kind.clone(),
                    detail: "supported kind without typed geometry (legacy payload could not be converted); the mask is preserved in the recipe and never silently dropped".to_string(),
                });
            }
        }
    }
    Ok(())
}

/// Rasterizes every visible mask container, in list order, into u8 masks at
/// the processing output size. Indexes align with the engine's
/// visible-mask adjustment order.
pub fn rasterize_visible_masks(
    masks: &[MaskContainer],
    frame: &MaskRasterFrame,
) -> Result<Vec<GrayImage>, MaskRasterError> {
    let mut bitmaps = Vec::new();
    for mask in masks.iter().filter(|mask| mask.visible) {
        let mut combined = GrayImage::new(frame.out_width, frame.out_height);
        for sub_mask in mask.sub_masks.iter() {
            if !sub_mask.visible {
                continue;
            }
            let geometry = sub_mask
                .geometry
                .as_ref()
                .ok_or_else(|| unsupported_error(mask, sub_mask, "missing typed geometry"))?;
            let mut layer = rasterize_sub_mask(geometry, sub_mask, frame)
                .map_err(|detail| invalid_geometry_error(mask, sub_mask, detail))?;
            if sub_mask.invert {
                for p in layer.pixels_mut() {
                    p[0] = 255 - p[0];
                }
            }
            let opacity_multiplier = (sub_mask.opacity / 100.0).clamp(0.0, 1.0) as f32;
            if opacity_multiplier < 1.0 {
                for pixel in layer.pixels_mut() {
                    pixel[0] = (f32::from(pixel[0]) * opacity_multiplier) as u8;
                }
            }
            combine(&mut combined, &layer, sub_mask.mode);
        }
        if mask.invert {
            for pixel in combined.pixels_mut() {
                pixel[0] = 255 - pixel[0];
            }
        }
        let opacity_multiplier = (mask.opacity / 100.0).clamp(0.0, 1.0) as f32;
        if opacity_multiplier < 1.0 {
            for pixel in combined.pixels_mut() {
                pixel[0] = (f32::from(pixel[0]) * opacity_multiplier) as u8;
            }
        }
        bitmaps.push(combined);
    }
    Ok(bitmaps)
}

fn unsupported_error(mask: &MaskContainer, sub_mask: &SubMask, detail: &str) -> MaskRasterError {
    MaskRasterError::Unsupported {
        mask_id: mask.id.clone(),
        sub_mask_id: sub_mask.id.clone(),
        kind: sub_mask.kind.clone(),
        detail: detail.to_string(),
    }
}

fn invalid_geometry_error(
    mask: &MaskContainer,
    sub_mask: &SubMask,
    detail: String,
) -> MaskRasterError {
    MaskRasterError::InvalidGeometry {
        mask_id: mask.id.clone(),
        sub_mask_id: sub_mask.id.clone(),
        detail,
    }
}

fn combine(dst: &mut GrayImage, src: &GrayImage, mode: SubMaskMode) {
    for (x, y, pixel) in dst.enumerate_pixels_mut() {
        let sub_pixel = src.get_pixel(x, y)[0];
        pixel[0] = match mode {
            SubMaskMode::Additive => pixel[0].max(sub_pixel),
            SubMaskMode::Subtractive => pixel[0].saturating_sub(sub_pixel),
            SubMaskMode::Intersect => pixel[0].min(sub_pixel),
        };
    }
}

fn rasterize_sub_mask(
    geometry: &MaskGeometry,
    sub_mask: &SubMask,
    frame: &MaskRasterFrame,
) -> Result<GrayImage, String> {
    let (width, height) = (frame.out_width, frame.out_height);
    let scale = frame.scale();
    let crop = frame.crop_offset();
    // Normalized schema coordinates convert to oriented pixels first, then
    // apply the reference dst = px * scale - crop * scale mapping.
    let px_w = |n: f64| (n * frame.oriented_width) as f32;
    let px_h = |n: f64| (n * frame.oriented_height) as f32;
    match geometry {
        MaskGeometry::All => Ok(GrayImage::from_pixel(width, height, Luma([255]))),
        MaskGeometry::Radial {
            center_x,
            center_y,
            radius_x,
            radius_y,
            rotation,
            feather,
        } => Ok(radial_bitmap(
            (px_w(*center_x), px_h(*center_y)),
            (px_w(*radius_x), px_w(*radius_y)),
            *rotation,
            *feather,
            (width, height),
            (scale, crop),
        )),
        MaskGeometry::Linear {
            start_x,
            start_y,
            end_x,
            end_y,
            range,
        } => Ok(linear_bitmap(
            (px_w(*start_x), px_h(*start_y)),
            (px_w(*end_x), px_h(*end_y)),
            px_w(*range),
            (width, height),
            (scale, crop),
        )),
        MaskGeometry::Brush { lines } => {
            let mapped: Vec<BrushLinePx> = lines
                .iter()
                .map(|line| BrushLinePx {
                    tool: line.tool,
                    radius: (px_w(line.brush_size) * scale / 2.0).max(0.0),
                    feather: (line.feather as f32).clamp(0.0, 1.0),
                    points: line
                        .points
                        .iter()
                        .map(|p| (px_w(p.x) * scale - crop.0, px_h(p.y) * scale - crop.1))
                        .collect(),
                })
                .collect();
            Ok(brush_bitmap(&mapped, width, height))
        }
        MaskGeometry::Flow { lines } => {
            let mapped: Vec<FlowLinePx> = lines
                .iter()
                .map(|line| FlowLinePx {
                    tool: line.tool,
                    radius: (px_w(line.brush_size) * scale / 2.0).max(0.0),
                    feather: (line.feather as f32).clamp(0.0, 1.0),
                    flow_per_stroke: ((line.flow as f32).clamp(0.0, 100.0) / 100.0) * 255.0,
                    points: line
                        .points
                        .iter()
                        .map(|p| (px_w(p.x) * scale - crop.0, px_h(p.y) * scale - crop.1))
                        .collect(),
                })
                .collect();
            Ok(flow_bitmap(&mapped, width, height))
        }
    }
    .map_err(|detail: String| format!("kind '{}': {detail}", sub_mask.kind))
}

/// Pixel-space stroke line ready for rasterization.
struct BrushLinePx {
    tool: BrushTool,
    radius: f32,
    feather: f32,
    points: Vec<(f32, f32)>,
}

/// Pixel-space flow stroke line.
struct FlowLinePx {
    tool: BrushTool,
    radius: f32,
    feather: f32,
    flow_per_stroke: f32,
    points: Vec<(f32, f32)>,
}

/// Reference `generate_radial_bitmap`, including its f32 pipeline and the
/// truncating center conversion. Inputs are oriented pixels; `map` applies
/// the reference `px * scale - crop` mapping.
fn radial_bitmap(
    center: (f32, f32),
    radii: (f32, f32),
    rotation: f64,
    feather: f64,
    size: (u32, u32),
    map: (f32, (f32, f32)),
) -> GrayImage {
    let (scale, crop) = map;
    let (width, height) = size;
    // `(value_px * scale - crop) as i32` then back to f32, exactly like the
    // reference's truncating center conversion.
    let center_px = (
        (center.0 * scale - crop.0) as i32 as f32,
        (center.1 * scale - crop.1) as i32 as f32,
    );
    let radius_x = radii.0 * scale;
    let radius_y = radii.1 * scale;
    let rotation_rad = (rotation as f32) * std::f32::consts::PI / 180.0;
    let mut mask = GrayImage::new(width, height);
    let cos_rot = rotation_rad.cos();
    let sin_rot = rotation_rad.sin();
    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - center_px.0;
            let dy = y as f32 - center_px.1;
            let rot_dx = dx * cos_rot + dy * sin_rot;
            let rot_dy = -dx * sin_rot + dy * cos_rot;
            let norm_x = rot_dx / radius_x.max(0.01);
            let norm_y = rot_dy / radius_y.max(0.01);
            let dist = (norm_x.powi(2) + norm_y.powi(2)).sqrt();
            let inner_bound = 1.0 - (feather as f32).clamp(0.0, 1.0);
            let intensity = 1.0 - (dist - inner_bound) / (1.0 - inner_bound).max(0.01);
            let clamped = intensity.clamp(0.0, 1.0);
            mask.put_pixel(x, y, Luma([(clamped * 255.0) as u8]));
        }
    }
    mask
}

/// Reference `generate_linear_bitmap`. A degenerate line yields an empty
/// (all-zero) mask. Inputs are oriented pixels; `map` = (scale, crop).
fn linear_bitmap(
    start: (f32, f32),
    end: (f32, f32),
    range: f32,
    size: (u32, u32),
    map: (f32, (f32, f32)),
) -> GrayImage {
    let (scale, crop) = map;
    let (width, height) = size;
    let mut mask = GrayImage::new(width, height);
    let start_x = start.0 * scale - crop.0;
    let start_y = start.1 * scale - crop.1;
    let end_x = end.0 * scale - crop.0;
    let end_y = end.1 * scale - crop.1;
    let range = range * scale;
    let line_vec_x = end_x - start_x;
    let line_vec_y = end_y - start_y;
    let len_sq = line_vec_x.powi(2) + line_vec_y.powi(2);
    if len_sq < 0.01 {
        return mask;
    }
    let perp_vec_x = -line_vec_y / len_sq.sqrt();
    let perp_vec_y = line_vec_x / len_sq.sqrt();
    let half_width = range.max(0.01);
    for y in 0..height {
        for x in 0..width {
            let pixel_vec_x = x as f32 - start_x;
            let pixel_vec_y = y as f32 - start_y;
            let dist_perp = pixel_vec_x * perp_vec_x + pixel_vec_y * perp_vec_y;
            let t = dist_perp / half_width;
            let intensity = (0.5 - t * 0.5).clamp(0.0, 1.0);
            mask.put_pixel(x, y, Luma([(intensity * 255.0) as u8]));
        }
    }
    mask
}

/// Reference `generate_brush_bitmap`: per-line stroke layers with
/// smoothstep feathering, union-composited (paint) or multiplied out
/// (eraser) in list order. Lines arrive in pixel space with radius/feather
/// precomputed.
fn brush_bitmap(lines: &[BrushLinePx], width: u32, height: u32) -> GrayImage {
    let mut final_mask = GrayImage::new(width, height);
    for line in lines {
        if line.points.is_empty() {
            continue;
        }
        let is_eraser = line.tool == BrushTool::Eraser;
        let layer = stroke_layer(&line.points, line.radius, line.feather, width, height);
        blend_brush_layer(&mut final_mask, &layer, is_eraser);
    }
    final_mask
}

/// Reference `generate_flow_bitmap`: brush geometry with per-stroke
/// accumulation.
fn flow_bitmap(lines: &[FlowLinePx], width: u32, height: u32) -> GrayImage {
    let mut final_mask = GrayImage::new(width, height);
    for line in lines {
        if line.points.is_empty() {
            continue;
        }
        let is_eraser = line.tool == BrushTool::Eraser;
        let layer = stroke_layer(&line.points, line.radius, line.feather, width, height);
        blend_flow_layer(&mut final_mask, &layer, is_eraser, line.flow_per_stroke);
    }
    final_mask
}

/// One stroke layer at full buffer size (the reference rasterizes into a
/// bounding box; values outside it are zero, so full-buffer output is
/// identical). Points arrive already mapped into output space.
fn stroke_layer(
    points: &[(f32, f32)],
    radius: f32,
    feather: f32,
    width: u32,
    height: u32,
) -> GrayImage {
    let mapped: &[(f32, f32)] = points;
    let inner_radius = radius * (1.0 - feather);
    let feather_range = (radius - inner_radius).max(0.01);
    let radius_sq = radius * radius;
    let inner_radius_sq = inner_radius * inner_radius;
    let mut layer = GrayImage::new(width, height);
    for (y, x) in (0..height).flat_map(|y| (0..width).map(move |x| (y, x))) {
        let px = x as f32;
        let py = y as f32;
        let mut min_dist_sq = radius_sq + 1.0;
        if mapped.len() == 1 {
            let (sx, sy) = mapped[0];
            min_dist_sq = min_dist_sq.min((px - sx) * (px - sx) + (py - sy) * (py - sy));
        } else {
            for pair in mapped.windows(2) {
                let (x1, y1) = pair[0];
                let (x2, y2) = pair[1];
                let dx = x2 - x1;
                let dy = y2 - y1;
                let len_sq = dx * dx + dy * dy;
                let dist_sq = if len_sq < 0.0001 {
                    (px - x1) * (px - x1) + (py - y1) * (py - y1)
                } else {
                    let t = (((px - x1) * dx + (py - y1) * dy) / len_sq).clamp(0.0, 1.0);
                    let proj_x = x1 + t * dx;
                    let proj_y = y1 + t * dy;
                    (px - proj_x) * (px - proj_x) + (py - proj_y) * (py - proj_y)
                };
                min_dist_sq = min_dist_sq.min(dist_sq);
            }
        }
        if min_dist_sq <= radius_sq {
            let intensity = if min_dist_sq <= inner_radius_sq {
                1.0
            } else {
                let dist = min_dist_sq.sqrt();
                let t = ((dist - inner_radius) / feather_range).clamp(0.0, 1.0);
                1.0 - (t * t * (3.0 - 2.0 * t))
            };
            layer.put_pixel(x, y, Luma([(intensity * 255.0).round() as u8]));
        }
    }
    layer
}

fn blend_brush_layer(final_mask: &mut GrayImage, layer: &GrayImage, is_eraser: bool) {
    for (x, y, dst_pixel) in final_mask.enumerate_pixels_mut() {
        let src_val = f32::from(layer.get_pixel(x, y)[0]) / 255.0;
        if src_val <= 0.0 {
            continue;
        }
        let dst_val = f32::from(dst_pixel[0]) / 255.0;
        let blended = if is_eraser {
            dst_val * (1.0 - src_val)
        } else {
            dst_val + src_val - dst_val * src_val
        };
        dst_pixel[0] = (blended.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
}

fn blend_flow_layer(
    final_mask: &mut GrayImage,
    layer: &GrayImage,
    is_eraser: bool,
    flow_per_stroke: f32,
) {
    for (x, y, pixel) in final_mask.enumerate_pixels_mut() {
        let stroke_pixel = f32::from(layer.get_pixel(x, y)[0]);
        if stroke_pixel <= 0.0 {
            continue;
        }
        let c_norm = f32::from(pixel[0]) / 255.0;
        let delta = (stroke_pixel / 255.0 * flow_per_stroke).round();
        let d_norm = (delta / 255.0).clamp(0.0, 1.0);
        let next = if is_eraser {
            c_norm * (1.0 - d_norm)
        } else {
            c_norm + d_norm - c_norm * d_norm
        };
        pixel[0] = (next.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
}

/// Bounded mask-bitmap cache. At most [`MASK_CACHE_MAX_ENTRIES`] bitmaps are
/// retained; inserting beyond the bound clears the cache wholesale, matching
/// the reference's memory-bound `mask_cache` behavior. Keys cover the
/// geometry content plus frame (size, scale, crop offset), so per-mask
/// adjustment edits do not invalidate geometry bitmaps while any geometry or
/// frame change does.
#[derive(Default)]
pub struct BoundedMaskCache {
    entries: HashMap<u64, Vec<GrayImage>>,
    capacity: usize,
}

impl BoundedMaskCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            capacity: MASK_CACHE_MAX_ENTRIES,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Cache key for one rasterization request.
    fn key(masks: &[MaskContainer], frame: &MaskRasterFrame) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for mask in masks.iter().filter(|mask| mask.visible) {
            mask.id.hash(&mut hasher);
            mask.invert.hash(&mut hasher);
            mask.opacity.to_bits().hash(&mut hasher);
            for sub_mask in mask.sub_masks.iter().filter(|s| s.visible) {
                sub_mask.id.hash(&mut hasher);
                sub_mask.invert.hash(&mut hasher);
                std::format!("{:?}", sub_mask.mode).hash(&mut hasher);
                sub_mask.opacity.to_bits().hash(&mut hasher);
                if let Some(geometry) = &sub_mask.geometry
                    && let Ok((kind, payload)) =
                        geometry.to_legacy(frame.oriented_width, frame.oriented_height)
                {
                    kind.hash(&mut hasher);
                    if let Ok(bytes) = serde_json::to_vec(&payload) {
                        bytes.hash(&mut hasher);
                    }
                }
            }
        }
        frame.oriented_width.to_bits().hash(&mut hasher);
        frame.oriented_height.to_bits().hash(&mut hasher);
        frame.crop_offset_x.to_bits().hash(&mut hasher);
        frame.crop_offset_y.to_bits().hash(&mut hasher);
        frame.out_width.hash(&mut hasher);
        frame.out_height.hash(&mut hasher);
        hasher.finish()
    }

    /// Returns cached bitmaps for this request or rasterizes, stores and
    /// returns them.
    pub fn get_or_rasterize(
        &mut self,
        masks: &[MaskContainer],
        frame: &MaskRasterFrame,
        rasterize: impl FnOnce() -> Result<Vec<GrayImage>, MaskRasterError>,
    ) -> Result<Vec<GrayImage>, MaskRasterError> {
        let key = Self::key(masks, frame);
        if let Some(bitmaps) = self.entries.get(&key) {
            return Ok(bitmaps.clone());
        }
        let bitmaps = rasterize()?;
        if self.entries.len() >= self.capacity {
            self.entries.clear();
        }
        self.entries.insert(key, bitmaps.clone());
        Ok(bitmaps)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
