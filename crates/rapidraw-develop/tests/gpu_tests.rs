//! Hardware-backed and pure tests for the offscreen GPU adjustment module
//! (`rapidraw-develop::gpu`), guarding Lap `docs/raw-development/spec.md`
//! A6 (processing order/numerics preserved), A7 (typed capability errors,
//! never an unprocessed base image as success) and A11 (no window/AI/AppState
//! dependency).
//!
//! Hardware-backed tests fail loudly when no GPU adapter is available: a
//! missing adapter is exactly the condition they exist to report.

use image::{ImageBuffer, Luma, Rgba, RgbaImage};
use rapidraw_develop::gpu::{
    AllAdjustments, GpuError, HistogramBins, OffscreenGpuContext, OffscreenRenderer,
    OutputColorSpace, OutputTarget, RenderRequest, get_all_adjustments_from_json,
    histogram_from_rendered,
};
use serde_json::json;

fn hw_context() -> OffscreenGpuContext {
    OffscreenGpuContext::new()
        .expect("hardware GPU adapter required: these tests verify real offscreen GPU behavior")
}

fn gradient_image(width: u32, height: u32) -> RgbaImage {
    ImageBuffer::from_fn(width, height, |x, y| {
        Rgba([
            ((x * 255) / width.max(1)) as u8,
            ((y * 255) / height.max(1)) as u8,
            (((x + y) * 255) / (width + height).max(1)) as u8,
            255,
        ])
    })
}

fn default_request<'a>() -> RenderRequest<'a> {
    RenderRequest {
        adjustments: AllAdjustments::default(),
        mask_bitmaps: &[],
        lut: None,
        roi: None,
    }
}

fn request_from_json(value: serde_json::Value) -> RenderRequest<'static> {
    RenderRequest {
        adjustments: get_all_adjustments_from_json(&value, false, None),
        mask_bitmaps: &[],
        lut: None,
        roi: None,
    }
}

fn assert_texture_too_large(err: &GpuError, width: u32, height: u32) {
    match err {
        GpuError::TextureTooLarge {
            width: w,
            height: h,
            max_dimension,
        } => {
            assert_eq!((*w, *h), (width, height));
            assert!(*max_dimension > 0);
        }
        other => panic!("expected TextureTooLarge, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A7: typed capability errors
// ---------------------------------------------------------------------------

#[test]
fn missing_adapter_is_a_typed_no_adapter_error() {
    let err = match OffscreenGpuContext::new_with_backends(wgpu::Backends::empty()) {
        Ok(_) => panic!("empty backend set must fail adapter selection"),
        Err(e) => e,
    };
    assert!(
        matches!(err, GpuError::NoAdapter(_)),
        "expected typed NoAdapter, got {err:?}"
    );
}

#[test]
fn texture_dimension_check_rejects_oversized_and_empty() {
    assert_texture_too_large(
        &GpuError::check_texture_dimensions(9000, 10, 8192).unwrap_err(),
        9000,
        10,
    );
    assert_texture_too_large(
        &GpuError::check_texture_dimensions(10, 8193, 8192).unwrap_err(),
        10,
        8193,
    );
    assert!(matches!(
        GpuError::check_texture_dimensions(0, 10, 8192),
        Err(GpuError::InvalidRequest(_))
    ));
    assert!(GpuError::check_texture_dimensions(8192, 8192, 8192).is_ok());
}

#[test]
fn oversized_texture_is_typed_error_not_unprocessed_base_image() {
    let context = hw_context();
    let max = context.limits().max_texture_dimension_2d;
    let renderer = OffscreenRenderer::new(context);
    let oversized = gradient_image(max + 1, 4);
    let err = renderer
        .render(
            &oversized.into(),
            7,
            default_request(),
            OutputTarget::CpuPixels,
        )
        .expect_err("oversized render must fail, never fall back to the base image");
    assert_texture_too_large(&err, max + 1, 4);
}

#[test]
fn has_lut_without_lut_resource_is_resource_missing() {
    let renderer = OffscreenRenderer::new(hw_context());
    let err = renderer
        .render(
            &gradient_image(32, 24).into(),
            1,
            request_from_json(json!({ "lutPath": "C:/nonexistent/resources/test.cube" })),
            OutputTarget::CpuPixels,
        )
        .expect_err("recipe references a LUT but none was resolved");
    match err {
        GpuError::ResourceMissing { kind, .. } => assert_eq!(kind, "lut"),
        other => panic!("expected ResourceMissing {{ kind: \"lut\" }}, got {other:?}"),
    }
}

#[test]
fn capabilities_report_the_device() {
    let context = hw_context();
    let capabilities = context.capabilities();
    assert!(
        capabilities.max_texture_dimension_2d >= 2048,
        "implausibly small max texture dimension"
    );
    assert!(!capabilities.adapter_name.is_empty());
    assert!(!capabilities.backend.is_empty());
    assert!(
        !context.is_device_lost(),
        "fresh context must not report device loss"
    );
}

// ---------------------------------------------------------------------------
// Section bypass, scaling and mask semantics (verbatim-port contracts)
// ---------------------------------------------------------------------------

#[test]
fn section_bypass_falls_back_to_neutral_defaults() {
    let visible = get_all_adjustments_from_json(
        &json!({ "exposure": 50.0, "vignetteMidpoint": 70.0, "sectionVisibility": {} }),
        false,
        None,
    );
    assert!((visible.global.exposure - 50.0 / 0.8).abs() < 1e-6);
    assert!((visible.global.vignette_midpoint - 0.7).abs() < 1e-6);

    let bypassed = get_all_adjustments_from_json(
        &json!({
            "exposure": 50.0,
            "contrast": 40.0,
            "vignetteMidpoint": 70.0,
            "grainSize": 80.0,
            "sectionVisibility": { "basic": false, "effects": false }
        }),
        false,
        None,
    );
    // Hidden section without a host default -> neutral zero.
    assert_eq!(bypassed.global.exposure, 0.0);
    assert_eq!(bypassed.global.contrast, 0.0);
    // Hidden section with a host default -> that default (50 -> 0.5).
    assert!((bypassed.global.vignette_midpoint - 0.5).abs() < 1e-6);
    assert!((bypassed.global.grain_size - 0.5).abs() < 1e-6);
    // LUT flag lives in the effects section: bypassed -> not requested.
    assert_eq!(bypassed.global.has_lut, 0);
}

#[test]
fn color_and_detail_scaling_matches_host_semantics() {
    let adj = get_all_adjustments_from_json(
        &json!({
            "temperature": 25.0,
            "sharpness": 50.0,
            "saturation": -100.0,
            "curves": { "luma": [ {"x": 0.0, "y": 0.0}, {"x": 128.0, "y": 118.0}, {"x": 255.0, "y": 255.0} ] }
        }),
        false,
        None,
    );
    assert!((adj.global.temperature - 1.0).abs() < 1e-6);
    assert!((adj.global.sharpness - 1.0).abs() < 1e-6);
    assert!((adj.global.saturation + 1.0).abs() < 1e-6);
    assert_eq!(adj.global.luma_curve_count, 3);
    assert!((adj.global.luma_curve[1].x - 128.0).abs() < 1e-6);
    // Unset channels fall back to the 2-point identity curve, exactly like
    // the pre-extraction host parser.
    assert_eq!(adj.global.red_curve_count, 2);
    assert!((adj.global.red_curve[0].x - 0.0).abs() < 1e-6);
    assert!((adj.global.red_curve[1].x - 255.0).abs() < 1e-6);
    assert_eq!(adj.global.tonemapper_mode, 0, "basic tone mapper is mode 0");
    let agx = get_all_adjustments_from_json(&json!({ "toneMapper": "agx" }), false, None);
    assert_eq!(agx.global.tonemapper_mode, 1, "agx tone mapper is mode 1");
}

fn mask_entry(visible: bool, adjustments: serde_json::Value) -> serde_json::Value {
    json!({
        "id": "m", "name": "m", "visible": visible, "invert": false,
        "adjustments": adjustments, "sub_masks": []
    })
}

#[test]
fn invisible_masks_are_skipped_and_visible_masks_are_ordered() {
    let adj = get_all_adjustments_from_json(
        &json!({
            "masks": [
                mask_entry(false, json!({ "exposure": 100.0 })),
                mask_entry(true, json!({ "exposure": 40.0 })),
                mask_entry(true, json!({}))
            ]
        }),
        false,
        None,
    );
    assert_eq!(adj.mask_count, 2, "invisible masks never occupy slots");
    // 40 / SCALES.exposure(0.8) == 50, matching the host scaling.
    assert!((adj.mask_adjustments[0].exposure - 50.0).abs() < 1e-5);
    assert_eq!(adj.mask_adjustments[1].exposure, 0.0);
}

#[test]
fn is_raw_flag_reaches_the_shader_uniform() {
    let raw = get_all_adjustments_from_json(&json!({}), true, None);
    assert_eq!(raw.global.is_raw_image, 1);
    let non_raw = get_all_adjustments_from_json(&json!({}), false, None);
    assert_eq!(non_raw.global.is_raw_image, 0);
}

// ---------------------------------------------------------------------------
// Histogram and color-transform contracts
// ---------------------------------------------------------------------------

#[test]
fn histogram_bins_follow_the_host_formula_exactly() {
    // Verbatim inline transcription of the pre-extraction host histogram
    // (RapidRAW image_processing.rs): counts over every second pixel, luma
    // as (r*218 + g*732 + b*74) >> 10, gaussian smoothing sigma 2, then
    // 99th-percentile normalization.
    fn host_formula(rgb8: &[u8]) -> HistogramBins {
        let mut acc = ([0u32; 256], [0u32; 256], [0u32; 256], [0u32; 256]);
        for pixel in rgb8.as_chunks::<3>().0.iter().step_by(2) {
            let r = pixel[0] as usize;
            let g = pixel[1] as usize;
            let b = pixel[2] as usize;
            acc.0[r] += 1;
            acc.1[g] += 1;
            acc.2[b] += 1;
            let luma = (r * 218 + g * 732 + b * 74) >> 10;
            acc.3[luma.min(255)] += 1;
        }
        let mut red: Vec<f32> = acc.0.into_iter().map(|c| c as f32).collect();
        let mut green: Vec<f32> = acc.1.into_iter().map(|c| c as f32).collect();
        let mut blue: Vec<f32> = acc.2.into_iter().map(|c| c as f32).collect();
        let mut luma: Vec<f32> = acc.3.into_iter().map(|c| c as f32).collect();
        for channel in [&mut red, &mut green, &mut blue, &mut luma] {
            rapidraw_develop::gpu::apply_gaussian_smoothing(channel, 2.0);
            rapidraw_develop::gpu::normalize_histogram_range(channel, 0.99);
        }
        HistogramBins {
            red,
            green,
            blue,
            luma,
        }
    }

    // 10x4 gradient with varied channel values so all 256-bin vectors are
    // non-trivial.
    let width = 10u32;
    let height = 4u32;
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    let mut rgb8 = Vec::with_capacity((width * height * 3) as usize);
    for i in 0..width * height {
        let p = [
            (i * 7 % 256) as u8,
            (i * 23 % 256) as u8,
            (i * 61 % 256) as u8,
            255,
        ];
        pixels.extend_from_slice(&p);
        rgb8.extend_from_slice(&p[..3]);
    }

    let produced =
        histogram_from_rendered(&pixels, width, height, OutputColorSpace::Rgba8Srgb).unwrap();
    let expected = host_formula(&rgb8);
    assert_eq!(produced.red, expected.red);
    assert_eq!(produced.green, expected.green);
    assert_eq!(produced.blue, expected.blue);
    assert_eq!(produced.luma, expected.luma);
}

#[test]
fn histogram_of_uniform_render_peaks_at_the_rendered_value() {
    // 16x8 uniform gray(128). Note the host formula's percentile
    // normalization saturates near-delta histograms into a plateau; the
    // correspondence contract is that the rendered value's bin is part of
    // the maximal plateau (value 1.0) and dominates its wider neighborhood.
    let width = 16u32;
    let height = 8u32;
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        pixels.extend_from_slice(&[128, 128, 128, 255]);
    }
    let bins =
        histogram_from_rendered(&pixels, width, height, OutputColorSpace::Rgba8Srgb).unwrap();
    for channel in [&bins.red, &bins.green, &bins.blue, &bins.luma] {
        assert_eq!(
            channel[128], 1.0,
            "rendered value's bin must sit on the normalized plateau"
        );
        for offset in [-6i64, 6] {
            let index = (128i64 + offset) as usize;
            assert!(
                channel[index] < 1.0,
                "bin {index} must fall below the plateau"
            );
        }
        assert!(*channel.last().unwrap() <= 1.0 + 1e-6);
        assert!(*channel.first().unwrap() <= 1.0 + 1e-6);
    }
}

#[test]
fn histogram_of_dynamic_image_matches_rendered_contract() {
    use rapidraw_develop::gpu::calculate_histogram_from_image;

    let img = gradient_image(20, 12);
    let from_image = calculate_histogram_from_image(&img.clone().into()).unwrap();
    let raw: Vec<u8> = img.pixels().flat_map(|p| p.0).collect();
    let from_rendered = histogram_from_rendered(&raw, 20, 12, OutputColorSpace::Rgba8Srgb).unwrap();
    assert_eq!(from_image.red, from_rendered.red);
    assert_eq!(from_image.green, from_rendered.green);
    assert_eq!(from_image.blue, from_rendered.blue);
    assert_eq!(from_image.luma, from_rendered.luma);
}

#[test]
fn histogram_rejects_short_pixel_buffers() {
    let err = histogram_from_rendered(&[0u8; 3], 4, 4, OutputColorSpace::Rgba8Srgb).unwrap_err();
    assert!(matches!(err, GpuError::InvalidRequest(_)));
}

// ---------------------------------------------------------------------------
// Hardware-backed ordered-pipeline regressions
// ---------------------------------------------------------------------------

#[test]
fn identity_render_is_numerically_neutral() {
    let renderer = OffscreenRenderer::new(hw_context());
    let base = gradient_image(64, 48);
    let output = renderer
        .render(
            &base.clone().into(),
            3,
            request_from_json(json!({})),
            OutputTarget::CpuPixels,
        )
        .expect("default-preset render must succeed on real hardware");
    assert_eq!((output.width, output.height), (64, 48));
    assert_eq!((output.x, output.y), (0, 0));
    assert_eq!(output.color_space(), OutputColorSpace::Rgba8Srgb);
    for (out, inp) in output.pixels.as_chunks::<4>().0.iter().zip(base.pixels()) {
        for c in 0..4 {
            let delta = (out[c] as i32 - inp[c] as i32).abs();
            assert!(
                delta <= 1,
                "identity pipeline drifted by {delta} at channel {c}"
            );
        }
    }
}

#[test]
fn adjusted_render_actually_processes_the_image() {
    // Guards the "unprocessed base image never counts as adjusted success"
    // invariant from the success side: exposure +0.5 must measurably brighten.
    let renderer = OffscreenRenderer::new(hw_context());
    let base = gradient_image(64, 48);
    let output = renderer
        .render(
            &base.clone().into(),
            5,
            request_from_json(json!({ "exposure": 40.0 })),
            OutputTarget::CpuPixels,
        )
        .expect("exposure render must succeed on real hardware");
    let mean = |pixels: &[u8]| -> f64 {
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[0] as f64)
            .sum::<f64>()
            / (pixels.len() / 4) as f64
    };
    assert!(
        mean(&output.pixels) > mean(base.as_raw()),
        "exposure render must change the image, not echo the base"
    );
}

#[test]
fn roi_render_preserves_geometry_and_matches_full_render() {
    let renderer = OffscreenRenderer::new(hw_context());
    let base = gradient_image(64, 48);
    let full = renderer
        .render(
            &base.clone().into(),
            9,
            request_from_json(json!({ "exposure": 40.0 })),
            OutputTarget::CpuPixels,
        )
        .expect("full render");
    let mut roi_request = request_from_json(json!({ "exposure": 40.0 }));
    roi_request.roi = Some(rapidraw_develop::gpu::Roi {
        x: 8,
        y: 8,
        width: 16,
        height: 16,
    });
    let roi = renderer
        .render(
            &base.clone().into(),
            9,
            roi_request,
            OutputTarget::CpuPixels,
        )
        .expect("ROI render");
    assert_eq!((roi.width, roi.height), (16, 16));
    assert_eq!((roi.x, roi.y), (8, 8));
    for row in 0..16u32 {
        for col in 0..16u32 {
            let full_index = (((row + 8) * 64 + (col + 8)) * 4) as usize;
            let roi_index = ((row * 16 + col) * 4) as usize;
            assert_eq!(
                &full.pixels[full_index..full_index + 4],
                &roi.pixels[roi_index..roi_index + 4],
                "ROI pixels must be geometrically identical to the full render"
            );
        }
    }
}

#[test]
fn mask_render_applies_mask_adjustments_regionally() {
    let renderer = OffscreenRenderer::new(hw_context());
    let base = gradient_image(64, 48);
    // Left half masked (255), right half unmasked (0).
    let mut mask = ImageBuffer::<Luma<u8>, Vec<u8>>::new(64, 48);
    for (x, _y, pixel) in mask.enumerate_pixels_mut() {
        *pixel = Luma([if x < 32 { 255 } else { 0 }]);
    }
    let mut request = request_from_json(json!({
        "masks": [ mask_entry(true, json!({ "exposure": 40.0 })) ]
    }));
    request.mask_bitmaps = std::slice::from_ref(&mask);
    let output = renderer
        .render(&base.clone().into(), 11, request, OutputTarget::CpuPixels)
        .expect("mask render");
    let mean_region = |x0: u32| -> f64 {
        let mut sum = 0.0;
        let mut count = 0u64;
        for y in 0..48u32 {
            for x in x0..x0 + 16 {
                let i = ((y * 64 + x) * 4) as usize;
                sum += output.pixels[i] as f64;
                count += 1;
            }
        }
        sum / count as f64
    };
    let left = mean_region(0);
    let right = mean_region(48);
    assert!(
        left > right + 1.0,
        "masked (left, {left}) region must be brighter than unmasked (right, {right})"
    );
}

#[test]
fn bytemuck_layouts_round_trip() {
    // The uniform block ABI: bytes_of must cover the same layout the shader
    // expects; a size change is a shader-semantics change.
    let adj = AllAdjustments::default();
    let bytes = bytemuck::bytes_of(&adj);
    assert_eq!(bytes.len(), std::mem::size_of::<AllAdjustments>());
    assert_eq!(bytes.len() % 16, 0, "uniform block stays 16-byte aligned");
}

#[test]
fn levels_process_endpoints_midtones_channels_and_bypass_on_gpu() {
    let renderer = OffscreenRenderer::new(hw_context());
    let base: image::DynamicImage =
        ImageBuffer::from_fn(256, 4, |x, _| Rgba([x as u8, x as u8, x as u8, 255])).into();
    let render = |value| {
        renderer
            .render(&base, 1, request_from_json(value), OutputTarget::CpuPixels)
            .unwrap()
            .pixels
    };
    let neutral = render(json!({}));
    let identity = render(json!({"levels":{"enabled":true}}));
    assert_eq!(neutral, identity, "neutral levels must be bit-identical");
    let lifted = render(
        json!({"levels":{"rgb":{"inputBlack":20,"inputWhite":220,"outputBlack":15,"outputWhite":240}}}),
    );
    for (input, expected) in [(20, 15), (120, 128), (220, 240), (230, 251)] {
        assert!(
            (lifted[input * 4] as i32 - expected).abs() <= 2,
            "input {input}: {} != {expected}",
            lifted[input * 4]
        );
    }
    let bypassed = render(json!({"levels":{"enabled":false,"rgb":{"outputBlack":50}}}));
    assert_eq!(bypassed, neutral);
    let red = render(json!({"levels":{"red":{"outputBlack":40,"outputWhite":220}}}));
    assert!((red[128 * 4] as i32 - 130).abs() <= 2);
    assert_eq!(red[128 * 4 + 1], neutral[128 * 4 + 1]);
    assert_eq!(red[128 * 4 + 2], neutral[128 * 4 + 2]);
    let bright = render(json!({"levels":{"rgb":{"midtone":0.5}}}));
    let dark = render(json!({"levels":{"rgb":{"midtone":-0.5}}}));
    assert!(bright[128 * 4] > neutral[128 * 4] + 20);
    assert!(dark[128 * 4] + 20 < neutral[128 * 4]);
    assert_eq!((bright[0], bright[255 * 4]), (0, 255));
    let composed = render(
        json!({"levels":{"rgb":{"outputBlack":20,"outputWhite":220},"red":{"outputBlack":30,"outputWhite":230}}}),
    );
    assert!((composed[128 * 4] as i32 - 124).abs() <= 2);
    assert!((composed[128 * 4 + 1] as i32 - 120).abs() <= 2);
}

#[test]
fn levels_individual_channels_and_existing_curves_compose_on_gpu() {
    let renderer = OffscreenRenderer::new(hw_context());
    let base: image::DynamicImage =
        ImageBuffer::from_pixel(4, 4, Rgba([128u8, 128, 128, 255])).into();
    for (channel, index) in [("red", 0), ("green", 1), ("blue", 2)] {
        let mut recipe = json!({"levels":{}});
        recipe["levels"][channel] = json!({"outputBlack":50,"outputWhite":240});
        let output = renderer
            .render(&base, 1, request_from_json(recipe), OutputTarget::CpuPixels)
            .unwrap();
        for c in 0..3 {
            let expected = if c == index { 145 } else { 128 };
            assert!(
                (output.pixels[c] as i32 - expected).abs() <= 2,
                "{channel}: channel {c}"
            );
        }
    }
    let output = renderer
        .render(
            &base,
            1,
            request_from_json(json!({
                "levels":{"rgb":{"outputBlack":20,"outputWhite":220}},
                "curves":{"luma":[{"x":0,"y":0},{"x":255,"y":127.5}]}
            })),
            OutputTarget::CpuPixels,
        )
        .unwrap();
    assert!(
        (output.pixels[0] as i32 - 60).abs() <= 2,
        "levels must precede, not replace, curves"
    );
}

#[test]
fn vignetting_ev_geometry_crop_anchor_and_roi_on_gpu() {
    let renderer = OffscreenRenderer::new(hw_context());
    let base: image::DynamicImage =
        ImageBuffer::from_pixel(160, 80, Rgba([128u8, 128, 128, 255])).into();
    let render = |value| {
        renderer
            .render(&base, 33, request_from_json(value), OutputTarget::CpuPixels)
            .unwrap()
            .pixels
    };
    let at = |v: &[u8], x: usize, y: usize| v[(y * 160 + x) * 4];
    let neutral = render(json!({"vignetteAmount":-20}));
    assert_eq!(
        neutral,
        render(json!({"vignetteAmount":-20,"vignetting":{"amount":0}}))
    );
    let plain = render(json!({}));
    let ellipse = render(json!({"vignetting":{"amount":-2,"method":"ellipticOnCrop"}}));
    assert!(
        at(&ellipse, 0, 40) + 30 < at(&plain, 0, 40),
        "negative EV must darken edge"
    );
    assert_eq!(
        at(&ellipse, 80, 40),
        at(&plain, 80, 40),
        "center stays unchanged"
    );
    assert!(
        (at(&ellipse, 0, 40) as i32 - at(&ellipse, 80, 0) as i32).abs() <= 2,
        "ellipse follows both dimensions"
    );
    let positive = render(json!({"vignetting":{"amount":2}}));
    assert!(at(&positive, 0, 40) > at(&plain, 0, 40) + 30);
    assert_eq!(
        plain,
        render(json!({"vignetting":{"enabled":false,"amount":-4}}))
    );
    let circle_value = json!({"vignetting":{"amount":-2,"method":"circular"}});
    let circle = render(circle_value.clone());
    assert!(
        at(&circle, 80, 0) > at(&ellipse, 80, 0) + 20,
        "circular geometry must differ on rectangular frame"
    );
    assert_eq!(
        circle,
        render(json!({"vignetting":{"amount":-2,"method":"circularOnCrop"}}))
    );
    let cropped: image::DynamicImage =
        ImageBuffer::from_pixel(80, 40, Rgba([128u8, 128, 128, 255])).into();
    let crop = json!({"x":0.25,"y":0.125,"width":0.5,"height":0.5});
    let mut value = circle_value.clone();
    value["crop"] = crop.clone();
    let anchored = renderer
        .render(
            &cropped,
            34,
            request_from_json(value.clone()),
            OutputTarget::CpuPixels,
        )
        .unwrap()
        .pixels;
    for y in 0..40 {
        for x in 0..80 {
            assert!(
                (anchored[(y * 80 + x) * 4] as i32 - at(&circle, x + 40, y + 10) as i32).abs() <= 1,
                "full-frame anchor at {x},{y}"
            );
        }
    }
    value["vignetting"]["method"] = json!("circularOnCrop");
    let relative = renderer
        .render(
            &cropped,
            34,
            request_from_json(value),
            OutputTarget::CpuPixels,
        )
        .unwrap()
        .pixels;
    assert_ne!(anchored, relative, "crop method recenters and rescales");
    let mut request = request_from_json(circle_value);
    request.roi = Some(rapidraw_develop::gpu::Roi {
        x: 32,
        y: 8,
        width: 64,
        height: 32,
    });
    let roi = renderer
        .render(&base, 33, request, OutputTarget::CpuPixels)
        .unwrap()
        .pixels;
    for y in 0..32 {
        for x in 0..64 {
            assert_eq!(roi[(y * 64 + x) * 4], at(&circle, x + 32, y + 8));
        }
    }
}
