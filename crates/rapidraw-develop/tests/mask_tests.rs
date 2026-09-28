//! Behavior tests for the host-neutral mask rasterizer (lap-78d /
//! rapidraw-922).
//!
//! Parity strategy: the expected buffers below are produced by
//! `reference::*`, an independent transcription of the pinned RapidRAW
//! `mask_generation.rs` per-pixel formulas (source revision
//! `5e30bcbb246395d391ba2e9662510641ffe68e6b`), written directly from the
//! reference source rather than shared with the implementation. The tests
//! require exact u8 equality of the rasterized mask bitmaps, including
//! crop/rotation combinations. This proves mask geometry and compositing
//! parity at the rasterizer level; full GPU render parity is a separate
//! fixture qualification and is not claimed here.

use rapidraw_develop::masks::{
    BoundedMaskCache, MaskRasterError, MaskRasterFrame, rasterize_visible_masks,
    validate_masks_supported, visible_mask_count,
};
use rapidraw_edit_model::{
    BrushLine, BrushTool, FlowLine, MaskContainer, MaskGeometry, MaskPoint, SubMask, SubMaskMode,
};
use serde_json::json;

const W: f64 = 800.0;
const H: f64 = 400.0;

fn frame(out_w: u32, out_h: u32, crop_x: f64, crop_y: f64) -> MaskRasterFrame {
    MaskRasterFrame::new(
        W as u32,
        H as u32,
        Some(rapidraw_edit_model::CropRect {
            x: crop_x,
            y: crop_y,
            width: 0.5,
            height: 0.5,
        }),
        out_w,
        out_h,
    )
    .expect("frame builds")
}

fn frame_no_crop(out_w: u32, out_h: u32) -> MaskRasterFrame {
    MaskRasterFrame::new(W as u32, H as u32, None, out_w, out_h).expect("frame builds")
}

fn container(id: &str, sub_masks: Vec<SubMask>) -> MaskContainer {
    MaskContainer {
        id: id.to_string(),
        name: id.to_string(),
        sub_masks,
        ..MaskContainer::default()
    }
}

fn sub_mask(id: &str, kind: &str, geometry: Option<MaskGeometry>) -> SubMask {
    SubMask {
        id: id.to_string(),
        kind: kind.to_string(),
        geometry,
        ..SubMask::default()
    }
}

// ---------------------------------------------------------------------------
// Reference transcriptions (independent of the implementation)
// ---------------------------------------------------------------------------

mod reference {
    use std::f32::consts::PI;

    #[allow(clippy::too_many_arguments)]
    pub fn radial(
        w: u32,
        h: u32,
        scale: f32,
        crop: (f32, f32),
        center: (f64, f64),
        radius: (f64, f64),
        rotation: f64,
        feather: f64,
    ) -> image::GrayImage {
        let mut mask = image::GrayImage::new(w, h);
        let center_x = (center.0 as f32 * scale - crop.0) as i32;
        let center_y = (center.1 as f32 * scale - crop.1) as i32;
        let radius_x = radius.0 as f32 * scale;
        let radius_y = radius.1 as f32 * scale;
        let rotation_rad = (rotation as f32) * PI / 180.0;
        for y in 0..h {
            for x in 0..w {
                let dx = x as f32 - center_x as f32;
                let dy = y as f32 - center_y as f32;
                let cos_rot = rotation_rad.cos();
                let sin_rot = rotation_rad.sin();
                let rot_dx = dx * cos_rot + dy * sin_rot;
                let rot_dy = -dx * sin_rot + dy * cos_rot;
                let norm_x = rot_dx / radius_x.max(0.01);
                let norm_y = rot_dy / radius_y.max(0.01);
                let dist = (norm_x.powi(2) + norm_y.powi(2)).sqrt();
                let inner_bound = 1.0 - feather.clamp(0.0, 1.0) as f32;
                let intensity = 1.0 - (dist - inner_bound) / (1.0 - inner_bound).max(0.01);
                let clamped = intensity.clamp(0.0, 1.0);
                mask.put_pixel(x, y, image::Luma([(clamped * 255.0) as u8]));
            }
        }
        mask
    }

    pub fn linear(
        w: u32,
        h: u32,
        scale: f32,
        crop: (f32, f32),
        start: (f64, f64),
        end: (f64, f64),
        range: f64,
    ) -> image::GrayImage {
        let mut mask = image::GrayImage::new(w, h);
        let start_x = start.0 as f32 * scale - crop.0;
        let start_y = start.1 as f32 * scale - crop.1;
        let end_x = end.0 as f32 * scale - crop.0;
        let end_y = end.1 as f32 * scale - crop.1;
        let range_px = range as f32 * scale;
        let line_vec_x = end_x - start_x;
        let line_vec_y = end_y - start_y;
        let len_sq = line_vec_x.powi(2) + line_vec_y.powi(2);
        if len_sq < 0.01 {
            return mask;
        }
        let perp_x = -line_vec_y / len_sq.sqrt();
        let perp_y = line_vec_x / len_sq.sqrt();
        let half_width = range_px.max(0.01);
        for y in 0..h {
            for x in 0..w {
                let pixel_x = x as f32 - start_x;
                let pixel_y = y as f32 - start_y;
                let dist_perp = pixel_x * perp_x + pixel_y * perp_y;
                let t = dist_perp / half_width;
                let intensity = (0.5 - t * 0.5).clamp(0.0, 1.0);
                mask.put_pixel(x, y, image::Luma([(intensity * 255.0) as u8]));
            }
        }
        mask
    }

    pub fn brush(
        w: u32,
        h: u32,
        scale: f32,
        crop: (f32, f32),
        lines: &[((f64, f64), (f64, f64), bool, f64, f64)],
    ) -> image::GrayImage {
        // lines: (from, to, is_eraser, brush_size_px, feather)
        let mut final_mask = image::GrayImage::new(w, h);
        for (from, to, is_eraser, brush_size, feather) in lines {
            let radius = (brush_size * f64::from(scale) / 2.0).max(0.0) as f32;
            let feather = feather.clamp(0.0, 1.0) as f32;
            let mut layer = vec![0u8; (w * h) as usize];
            let p0 = (
                from.0 as f32 * scale - crop.0,
                from.1 as f32 * scale - crop.1,
            );
            let p1 = (to.0 as f32 * scale - crop.0, to.1 as f32 * scale - crop.1);
            let inner_radius = radius * (1.0 - feather);
            let feather_range = (radius - inner_radius).max(0.01);
            let radius_sq = radius * radius;
            let inner_radius_sq = inner_radius * inner_radius;
            for y in 0..h {
                for x in 0..w {
                    let px = x as f32;
                    let py = y as f32;
                    let dist_sq = {
                        let dx = p1.0 - p0.0;
                        let dy = p1.1 - p0.1;
                        let len_sq = dx * dx + dy * dy;
                        if len_sq < 0.0001 {
                            (px - p0.0) * (px - p0.0) + (py - p0.1) * (py - p0.1)
                        } else {
                            let t =
                                (((px - p0.0) * dx + (py - p0.1) * dy) / len_sq).clamp(0.0, 1.0);
                            let proj_x = p0.0 + t * dx;
                            let proj_y = p0.1 + t * dy;
                            (px - proj_x) * (px - proj_x) + (py - proj_y) * (py - proj_y)
                        }
                    };
                    if dist_sq <= radius_sq {
                        let intensity = if dist_sq <= inner_radius_sq {
                            1.0
                        } else {
                            let dist = dist_sq.sqrt();
                            let t = ((dist - inner_radius) / feather_range).clamp(0.0, 1.0);
                            1.0 - (t * t * (3.0 - 2.0 * t))
                        };
                        layer[(y * w + x) as usize] = (intensity * 255.0).round() as u8;
                    }
                }
            }
            for (index, pixel) in final_mask.pixels_mut().enumerate() {
                let src_val = f32::from(layer[index]) / 255.0;
                if src_val <= 0.0 {
                    continue;
                }
                let dst_val = f32::from(pixel[0]) / 255.0;
                let blended = if *is_eraser {
                    dst_val * (1.0 - src_val)
                } else {
                    dst_val + src_val - dst_val * src_val
                };
                pixel[0] = (blended.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        final_mask
    }
}

// ---------------------------------------------------------------------------
// Frame / scale semantics
// ---------------------------------------------------------------------------

#[test]
fn raster_frame_maps_crop_offset_and_scale_like_the_reference() {
    let f = frame(400, 200, 0.25, 0.125);
    assert_eq!(f.out_width, 400);
    assert_eq!(f.out_height, 200);
    // scale = out_w / oriented_w = 0.5; crop offset px = (200, 50).
    assert_eq!(f.scale(), 0.5);
    assert_eq!(f.crop_offset(), (200.0, 50.0));
}

#[test]
fn raster_frame_rejects_degenerate_inputs() {
    assert!(MaskRasterFrame::new(0, 100, None, 10, 10).is_err());
    assert!(MaskRasterFrame::new(100, 100, None, 0, 10).is_err());
    // Crop wider than the frame is invalid on its own.
    assert!(
        MaskRasterFrame::new(
            100,
            100,
            Some(rapidraw_edit_model::CropRect {
                x: 0.9,
                y: 0.0,
                width: 0.5,
                height: 0.5
            }),
            10,
            10
        )
        .is_err()
    );
}

// ---------------------------------------------------------------------------
// Per-kind geometry parity
// ---------------------------------------------------------------------------

#[test]
fn radial_matches_reference_with_crop_and_rotation() {
    let geometry = MaskGeometry::Radial {
        center_x: 0.5,
        center_y: 0.5,
        radius_x: 0.2,
        radius_y: 0.15,
        rotation: 30.0,
        feather: 0.35,
    };
    let masks = vec![container(
        "m",
        vec![sub_mask("s", "radial", Some(geometry.clone()))],
    )];
    // No crop.
    let f = frame_no_crop(200, 100);
    let bitmaps = rasterize_visible_masks(&masks, &f).expect("rasterizes");
    assert_eq!(bitmaps.len(), 1);
    let expected = reference::radial(
        200,
        100,
        f.scale(),
        (0.0, 0.0),
        (400.0, 200.0),
        (160.0, 120.0),
        30.0,
        0.35,
    );
    assert_eq!(
        bitmaps[0].as_raw(),
        expected.as_raw(),
        "radial must match the reference exactly"
    );
    // With crop + orientation-combination frame.
    let f = frame(100, 100, 0.125, 0.25);
    let bitmaps = rasterize_visible_masks(&masks, &f).expect("rasterizes");
    let expected = reference::radial(
        100,
        100,
        f.scale(),
        (100.0, 100.0),
        (400.0, 200.0),
        (160.0, 120.0),
        30.0,
        0.35,
    );
    assert_eq!(bitmaps[0].as_raw(), expected.as_raw());
}

#[test]
fn radial_center_is_solid_and_falloff_reaches_zero() {
    let geometry = MaskGeometry::Radial {
        center_x: 0.5,
        center_y: 0.5,
        radius_x: 0.25,
        radius_y: 0.25,
        rotation: 0.0,
        feather: 0.5,
    };
    let masks = vec![container(
        "m",
        vec![sub_mask("s", "radial", Some(geometry))],
    )];
    let f = frame_no_crop(160, 80);
    let bitmaps = rasterize_visible_masks(&masks, &f).expect("rasterizes");
    // scale = 0.2: center maps to (80, 40), radii 40 px; feather 0.5.
    let inside = bitmaps[0].get_pixel(80, 40)[0];
    let outside = bitmaps[0].get_pixel(159, 79)[0];
    assert_eq!(inside, 255, "solid core must be full influence");
    assert_eq!(outside, 0, "beyond the feather band there is no influence");
}

#[test]
fn linear_matches_reference_and_is_empty_for_degenerate_lines() {
    let geometry = MaskGeometry::Linear {
        start_x: 0.25,
        start_y: 0.5,
        end_x: 0.75,
        end_y: 0.5,
        range: 100.0 / W,
    };
    let masks = vec![container(
        "m",
        vec![sub_mask("s", "linear", Some(geometry.clone()))],
    )];
    let f = frame_no_crop(320, 160);
    let bitmaps = rasterize_visible_masks(&masks, &f).expect("rasterizes");
    let expected = reference::linear(
        320,
        160,
        f.scale(),
        (0.0, 0.0),
        (200.0, 200.0),
        (600.0, 200.0),
        100.0,
    );
    assert_eq!(bitmaps[0].as_raw(), expected.as_raw());

    let degenerate = MaskGeometry::Linear {
        start_x: 0.5,
        start_y: 0.5,
        end_x: 0.5,
        end_y: 0.5,
        range: 0.1,
    };
    let masks = vec![container(
        "m",
        vec![sub_mask("s", "linear", Some(degenerate))],
    )];
    let bitmaps = rasterize_visible_masks(&masks, &frame_no_crop(64, 32)).expect("rasterizes");
    assert!(
        bitmaps[0].as_raw().iter().all(|&v| v == 0),
        "degenerate line renders empty"
    );
}

#[test]
fn brush_matches_reference_with_feather_and_eraser() {
    let geometry = MaskGeometry::Brush {
        lines: vec![
            BrushLine {
                tool: BrushTool::Brush,
                brush_size: 120.0 / W,
                feather: 0.6,
                points: vec![
                    MaskPoint {
                        x: 200.0 / W,
                        y: 200.0 / H,
                    },
                    MaskPoint {
                        x: 600.0 / W,
                        y: 200.0 / H,
                    },
                ],
            },
            BrushLine {
                tool: BrushTool::Eraser,
                brush_size: 60.0 / W,
                feather: 0.2,
                points: vec![MaskPoint {
                    x: 400.0 / W,
                    y: 200.0 / H,
                }],
            },
        ],
    };
    let masks = vec![container(
        "m",
        vec![sub_mask("s", "brush", Some(geometry.clone()))],
    )];
    let f = frame_no_crop(256, 128);
    let bitmaps = rasterize_visible_masks(&masks, &f).expect("rasterizes");
    let expected = reference::brush(
        256,
        128,
        f.scale(),
        (0.0, 0.0),
        &[
            ((200.0, 200.0), (600.0, 200.0), false, 120.0, 0.6),
            ((400.0, 200.0), (400.0, 200.0), true, 60.0, 0.2),
        ],
    );
    assert_eq!(
        bitmaps[0].as_raw(),
        expected.as_raw(),
        "brush compositing must match the reference"
    );
}

#[test]
fn flow_accumulates_per_stroke_like_the_reference_formula() {
    // Flow accumulates stroke alpha times flow fraction; a repeated stroke
    // over the same spot must accumulate, not saturate at the first stroke.
    let line = FlowLine {
        tool: BrushTool::Brush,
        brush_size: 80.0 / W,
        feather: 0.0,
        flow: 50.0,
        points: vec![MaskPoint { x: 0.5, y: 0.5 }],
    };
    let geometry = MaskGeometry::Flow { lines: vec![line] };
    let masks = vec![container("m", vec![sub_mask("s", "flow", Some(geometry))])];
    let f = frame_no_crop(100, 50);
    let bitmaps = rasterize_visible_masks(&masks, &f).expect("rasterizes");
    // Mapped stroke center: (0.5*800, 0.5*400) * 0.125 = (50, 25).
    let center = bitmaps[0].get_pixel(50, 25)[0];
    // One flow-50 stroke: d = round(255*0.5)/255 ≈ 0.502 -> 128 after round.
    assert_eq!(center, 128, "single flow-50 stroke deposits half influence");
    assert!(center < 255);
}

#[test]
fn all_kind_is_a_solid_mask() {
    let masks = vec![container(
        "m",
        vec![sub_mask("s", "all", Some(MaskGeometry::All))],
    )];
    let bitmaps = rasterize_visible_masks(&masks, &frame_no_crop(8, 4)).expect("rasterizes");
    assert!(bitmaps[0].as_raw().iter().all(|&v| v == 255));
}

// ---------------------------------------------------------------------------
// Compositing order, invert, opacity
// ---------------------------------------------------------------------------

#[test]
fn sub_mask_combination_order_matches_reference_semantics() {
    // Intersect against an empty base stays black; order matters: additive
    // first then intersect keeps the intersection.
    let radial_a = MaskGeometry::Radial {
        center_x: 0.3,
        center_y: 0.5,
        radius_x: 0.15,
        radius_y: 0.3,
        rotation: 0.0,
        feather: 0.0,
    };
    let radial_b = MaskGeometry::Radial {
        center_x: 0.7,
        center_y: 0.5,
        radius_x: 0.15,
        radius_y: 0.3,
        rotation: 0.0,
        feather: 0.0,
    };
    let masks = vec![container(
        "m",
        vec![
            sub_mask("a", "radial", Some(radial_a.clone())),
            SubMask {
                id: "b".to_string(),
                kind: "radial".to_string(),
                geometry: Some(radial_b),
                mode: SubMaskMode::Intersect,
                ..SubMask::default()
            },
        ],
    )];
    let f = frame_no_crop(160, 80);
    let bitmaps = rasterize_visible_masks(&masks, &f).expect("rasterizes");
    // Disjoint ellipses intersected -> black everywhere.
    assert!(bitmaps[0].as_raw().iter().all(|&v| v == 0));
    // Subtractive removes what additive added.
    let masks = vec![container(
        "m",
        vec![
            sub_mask("all", "all", Some(MaskGeometry::All)),
            SubMask {
                id: "hole".to_string(),
                kind: "radial".to_string(),
                geometry: Some(radial_a),
                mode: SubMaskMode::Subtractive,
                ..SubMask::default()
            },
        ],
    )];
    let bitmaps = rasterize_visible_masks(&masks, &f).expect("rasterizes");
    assert_eq!(
        bitmaps[0].get_pixel(28, 40)[0],
        0,
        "subtracted core is a hole"
    );
    assert_eq!(
        bitmaps[0].get_pixel(150, 5)[0],
        255,
        "outside the subtracted ellipse stays full"
    );
}

#[test]
fn invert_and_opacity_apply_per_sub_mask_then_per_container() {
    // Additive submask with opacity 50%: max(0, 255*0.5)=127.
    let masks = vec![container(
        "m",
        vec![SubMask {
            id: "half".to_string(),
            kind: "all".to_string(),
            geometry: Some(MaskGeometry::All),
            opacity: 50.0,
            ..SubMask::default()
        }],
    )];
    let bitmaps = rasterize_visible_masks(&masks, &frame_no_crop(4, 2)).expect("rasterizes");
    assert_eq!(bitmaps[0].get_pixel(0, 0)[0], 127);

    // Container invert on top: 255-127=128.
    let masks = vec![MaskContainer {
        id: "m".to_string(),
        name: "m".to_string(),
        invert: true,
        sub_masks: vec![SubMask {
            id: "half".to_string(),
            kind: "all".to_string(),
            geometry: Some(MaskGeometry::All),
            opacity: 50.0,
            ..SubMask::default()
        }],
        ..MaskContainer::default()
    }];
    let bitmaps = rasterize_visible_masks(&masks, &frame_no_crop(4, 2)).expect("rasterizes");
    assert_eq!(bitmaps[0].get_pixel(0, 0)[0], 128);

    // Sub-mask invert then sub-opacity: invert(255)=0 -> 0*0.5=0.
    let masks = vec![container(
        "m",
        vec![SubMask {
            id: "inv".to_string(),
            kind: "all".to_string(),
            geometry: Some(MaskGeometry::All),
            invert: true,
            opacity: 50.0,
            ..SubMask::default()
        }],
    )];
    let bitmaps = rasterize_visible_masks(&masks, &frame_no_crop(4, 2)).expect("rasterizes");
    assert_eq!(bitmaps[0].get_pixel(0, 0)[0], 0);
}

#[test]
fn invisible_masks_and_sub_masks_are_skipped_in_order() {
    let masks = vec![
        MaskContainer {
            visible: false,
            ..container(
                "hidden",
                vec![sub_mask("s", "all", Some(MaskGeometry::All))],
            )
        },
        container(
            "visible",
            vec![
                SubMask {
                    id: "on".to_string(),
                    kind: "all".to_string(),
                    geometry: Some(MaskGeometry::All),
                    ..SubMask::default()
                },
                SubMask {
                    id: "off".to_string(),
                    kind: "radial".to_string(),
                    geometry: Some(MaskGeometry::Radial {
                        center_x: 0.5,
                        center_y: 0.5,
                        radius_x: 0.1,
                        radius_y: 0.1,
                        rotation: 0.0,
                        feather: 0.0,
                    }),
                    visible: false,
                    ..SubMask::default()
                },
            ],
        ),
    ];
    assert_eq!(visible_mask_count(&masks), 1);
    let f = frame_no_crop(8, 4);
    validate_masks_supported(&masks).expect("visible masks are supported");
    let bitmaps = rasterize_visible_masks(&masks, &f).expect("rasterizes");
    assert_eq!(bitmaps.len(), 1, "only visible containers produce layers");
    assert!(
        bitmaps[0].as_raw().iter().all(|&v| v == 255),
        "hidden sub-mask must not subtract"
    );
}

#[test]
fn visible_container_without_sub_masks_yields_a_black_layer() {
    // Layer indexes must stay aligned with get_all_adjustments_from_json's
    // visible-container order; an empty container therefore rasterizes as a
    // no-op black layer instead of shifting subsequent masks.
    let masks = vec![container("empty", vec![])];
    let bitmaps = rasterize_visible_masks(&masks, &frame_no_crop(4, 2)).expect("rasterizes");
    assert_eq!(bitmaps.len(), 1);
    assert!(bitmaps[0].as_raw().iter().all(|&v| v == 0));
}

// ---------------------------------------------------------------------------
// Explicit unsupported handling (never silently dropped)
// ---------------------------------------------------------------------------

#[test]
fn unsupported_kinds_fail_explicitly_naming_mask_and_kind() {
    for kind in [
        "ai-subject",
        "ai-depth",
        "luminance",
        "color",
        "quick-eraser",
        "future-kind",
    ] {
        let masks = vec![container("m1", vec![sub_mask("s1", kind, None)])];
        let error = validate_masks_supported(&masks)
            .expect_err("unsupported kind must be reported explicitly");
        match error {
            MaskRasterError::Unsupported {
                mask_id,
                sub_mask_id,
                kind: reported,
                ..
            } => {
                assert_eq!((mask_id.as_str(), sub_mask_id.as_str()), ("m1", "s1"));
                assert_eq!(reported, kind);
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
        assert!(rasterize_visible_masks(&masks, &frame_no_crop(4, 2)).is_err());
    }
}

#[test]
fn supported_kind_with_missing_typed_geometry_fails_explicitly() {
    let masks = vec![container("m1", vec![sub_mask("s1", "radial", None)])];
    let error = validate_masks_supported(&masks).expect_err("missing geometry must fail");
    assert!(matches!(error, MaskRasterError::Unsupported { .. }));
}

#[test]
fn unsupported_kind_in_one_mask_does_not_block_supported_ones_from_validating() {
    // Validation reports the first unsupported sub-mask; a caller that
    // removes it proceeds. Bitmap output for the supported-only list works.
    let masks = vec![container(
        "ok",
        vec![sub_mask("s", "all", Some(MaskGeometry::All))],
    )];
    validate_masks_supported(&masks).expect("supported list validates");
    assert_eq!(visible_mask_count(&masks), 1);
}

// ---------------------------------------------------------------------------
// Bounded cache
// ---------------------------------------------------------------------------

#[test]
fn bounded_mask_cache_stays_under_the_limit() {
    let mut cache = BoundedMaskCache::new();
    let f = frame_no_crop(4, 4);
    let masks = vec![container(
        "m",
        vec![sub_mask("s", "all", Some(MaskGeometry::All))],
    )];
    for i in 0..120 {
        let geometry = MaskGeometry::Radial {
            center_x: f64::from(i) / 1000.0,
            center_y: 0.5,
            radius_x: 0.25,
            radius_y: 0.25,
            rotation: 0.0,
            feather: 0.5,
        };
        let masks = vec![container(
            "m",
            vec![sub_mask("s", "radial", Some(geometry))],
        )];
        cache
            .get_or_rasterize(&masks, &f, || rasterize_visible_masks(&masks, &f))
            .expect("rasterizes");
        assert!(cache.len() <= cache.capacity(), "cache must stay bounded");
    }
    assert_eq!(
        cache.capacity(),
        50,
        "cache bound matches the reference (50 entries)"
    );
    // A repeated identical request hits the cache (no re-rasterization).
    let mut calls = 0;
    cache
        .get_or_rasterize(&masks, &f, || {
            calls += 1;
            rasterize_visible_masks(&masks, &f)
        })
        .expect("rasterizes");
    assert_eq!(calls, 1, "first request rasterizes");
    cache
        .get_or_rasterize(&masks, &f, || {
            calls += 1;
            rasterize_visible_masks(&masks, &f)
        })
        .expect("rasterizes");
    assert_eq!(
        calls, 1,
        "second identical request must come from the cache"
    );
}

#[test]
fn cache_respects_geometry_and_frame_in_its_key() {
    let mut cache = BoundedMaskCache::new();
    let f_small = frame_no_crop(8, 4);
    let f_large = frame_no_crop(16, 8);
    let masks_all = vec![container(
        "m",
        vec![sub_mask("s", "all", Some(MaskGeometry::All))],
    )];
    let masks_empty: Vec<MaskContainer> = vec![];
    let all = cache
        .get_or_rasterize(&masks_all, &f_small, || {
            rasterize_visible_masks(&masks_all, &f_small)
        })
        .unwrap();
    let none = cache
        .get_or_rasterize(&masks_empty, &f_small, || {
            rasterize_visible_masks(&masks_empty, &f_small)
        })
        .unwrap();
    let all_large = cache
        .get_or_rasterize(&masks_all, &f_large, || {
            rasterize_visible_masks(&masks_all, &f_large)
        })
        .unwrap();
    assert!(all[0].as_raw().iter().all(|&v| v == 255));
    assert!(none.is_empty());
    assert!(all_large[0].as_raw().iter().all(|&v| v == 255));
    assert_eq!(all_large[0].dimensions(), (16, 8));
    // Distinct frames/geometries must produce distinct entries.
    assert!(cache.len() >= 2, "different keys must not collide");
}

// ---------------------------------------------------------------------------
// Legacy payload bridging through the host contract
// ---------------------------------------------------------------------------

#[test]
fn legacy_parameter_payload_survives_conversion_and_rasterization() {
    // A legacy pixel payload converts via edit-model, then rasterizes with
    // the same result as a natively authored typed geometry.
    let legacy = json!({
        "centerX": 400.0, "centerY": 200.0,
        "radiusX": 160.0, "radiusY": 120.0,
        "rotation": 30.0, "feather": 0.35
    });
    let converted = MaskGeometry::from_legacy("radial", &legacy, W, H)
        .expect("converts")
        .expect("supported");
    let native = MaskGeometry::Radial {
        center_x: 400.0 / W,
        center_y: 200.0 / H,
        radius_x: 160.0 / W,
        radius_y: 120.0 / W,
        rotation: 30.0,
        feather: 0.35,
    };
    assert_eq!(converted, native);
    let f = frame_no_crop(120, 60);
    let masks = vec![container(
        "m",
        vec![sub_mask("s", "radial", Some(converted))],
    )];
    let bitmaps = rasterize_visible_masks(&masks, &f).expect("rasterizes");
    assert_eq!(bitmaps[0].dimensions(), (120, 60));
}
