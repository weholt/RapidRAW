//! Lens correction (lap-d52 / rapidraw-50c): profile parsing, parameter
//! resolution, and the host-neutral lens geometry warp.
//!
//! Parity strategy mirrors mask_tests: exact comparisons against an
//! independent transcription of the pinned RapidRAW reference (revision
//! `5e30bcbb246395d391ba2e9662510641ffe68e6b`,
//! `src-tauri/src/image_processing.rs::warp_image_geometry` +
//! `compute_lens_auto_crop_scale` + `lens_correction.rs`). One deliberate,
//! documented tightening: an unsupported distortion model is an explicit
//! capability error instead of the reference's silent zeroing (spec A7 —
//! never silently change export).

use rapidraw_develop::LinearImage;
use rapidraw_develop::lens::{
    LensDatabase, LensError, LensWarpParams, find_best_lens_match, lens_auto_crop_scale, lens_warp,
    parse_lensfun_db, resolve_lens_params,
};

// ---------------------------------------------------------------------------
// Independent transcription of the reference warp for one output pixel with
// an identity perspective transform (no rotate/aspect/scale/offset inputs in
// this engine slice). Mirrors warp_image_geometry's per-pixel path exactly:
// f64 radial math on f32 coordinates, truncating bilinear edges (black when
// outside [0, w-1]x[0, h-1] on the non-TCA path, clamped on the TCA path).
// ---------------------------------------------------------------------------

struct RefWarp {
    width: usize,
    height: usize,
    cx: f32,
    cy: f32,
    hd: f64,
    max_radius_sq_inv: f64,
    k_distortion: f64,
    lk: [f64; 3],
    lens_dist_amt: f64,
    has_lens_correction: bool,
    is_ptlens: bool,
    auto_crop_scale: f32,
    vr: f32,
    vb: f32,
    has_tca: bool,
    vk: [f64; 3],
    lens_vig_amt: f64,
    has_vignetting: bool,
}

impl RefWarp {
    fn new(width: u32, height: u32, p: &LensWarpParams) -> Self {
        let (w, h) = (width as f32, height as f32);
        let cx = w / 2.0;
        let cy = h / 2.0;
        let hd = ((f64::from(width) * f64::from(width) + f64::from(height) * f64::from(height))
            .sqrt())
            / 2.0;
        let k_distortion = p.distortion / 100.0 * 2.5;
        let lens_dist_amt = p.lens_distortion_amount * 2.5;
        let has_lens_correction = p.lens_distortion_enabled
            && (p.lens_dist_k1.abs() > 1e-6
                || p.lens_dist_k2.abs() > 1e-6
                || p.lens_dist_k3.abs() > 1e-6);
        let vr = if (p.tca_vr as f32 - 1.0).abs() > 1e-5 {
            p.tca_vr as f32 + (1.0 - p.tca_vr as f32) * (1.0 - p.lens_tca_amount as f32)
        } else {
            1.0
        };
        let vb = if (p.tca_vb as f32 - 1.0).abs() > 1e-5 {
            p.tca_vb as f32 + (1.0 - p.tca_vb as f32) * (1.0 - p.lens_tca_amount as f32)
        } else {
            1.0
        };
        let has_tca = p.lens_tca_enabled && ((vr - 1.0).abs() > 1e-5 || (vb - 1.0).abs() > 1e-5);
        let lens_vig_amt = p.lens_vignette_amount * 0.8;
        let has_vignetting = p.lens_vignette_enabled
            && (p.vig_k1.abs() > 1e-6 || p.vig_k2.abs() > 1e-6 || p.vig_k3.abs() > 1e-6)
            && lens_vig_amt > 0.01;
        RefWarp {
            width: width as usize,
            height: height as usize,
            cx,
            cy,
            hd,
            // Reference computes the center radius product in f32, then casts.
            max_radius_sq_inv: 1.0 / ((cx * cx + cy * cy) as f64),
            k_distortion,
            lk: [p.lens_dist_k1, p.lens_dist_k2, p.lens_dist_k3],
            lens_dist_amt,
            has_lens_correction,
            is_ptlens: p.lens_model == 1,
            auto_crop_scale: lens_auto_crop_scale(width, height, p) as f32,
            vr,
            vb,
            has_tca,
            vk: [p.vig_k1, p.vig_k2, p.vig_k3],
            lens_vig_amt,
            has_vignetting,
        }
    }

    /// `compute_lens_auto_crop_scale` transcription (f64 pipeline).
    fn auto_crop_scale(&self) -> f64 {
        let (cx, cy) = (f64::from(self.cx), f64::from(self.cy));
        let width = f64::from(self.width as u32);
        let height = f64::from(self.height as u32);
        let half_diagonal = (cx * cx + cy * cy).sqrt();
        let max_radius_sq_inv = 1.0 / (cx * cx + cy * cy);
        let (lk1, lk2, lk3) = (self.lk[0], self.lk[1], self.lk[2]);
        let lens_dist_amt = self.lens_dist_amt;
        let k_distortion = self.k_distortion;
        let has_lens_correction = self.has_lens_correction;
        let is_ptlens = self.is_ptlens;

        let sample_points: [(f64, f64); 8] = [
            (cx, 0.0),
            (cx, height),
            (0.0, cy),
            (width, cy),
            (0.0, 0.0),
            (width, 0.0),
            (0.0, height),
            (width, height),
        ];
        let mut max_scale: f64 = 1.0;
        for &(px, py) in &sample_points {
            let dx = px - cx;
            let dy = py - cy;
            let ru = (dx * dx + dy * dy).sqrt();
            if ru < 1e-6 {
                continue;
            }
            let mut mapped_dx = dx;
            let mut mapped_dy = dy;
            if has_lens_correction {
                let ru_norm = ru / half_diagonal;
                let ru_norm2 = ru_norm * ru_norm;
                let rd_norm = if is_ptlens {
                    let (a, b, c) = (lk1, lk2, lk3);
                    let d = 1.0 - a - b - c;
                    ru_norm * (a * ru_norm2 * ru_norm + b * ru_norm2 + c * ru_norm + d)
                } else {
                    ru_norm
                        * (1.0
                            + lk1 * ru_norm2
                            + lk2 * (ru_norm2 * ru_norm2)
                            + lk3 * (ru_norm2 * ru_norm2 * ru_norm2))
                };
                let effective_r_norm = ru_norm + (rd_norm - ru_norm) * lens_dist_amt;
                let scale = effective_r_norm / ru_norm;
                mapped_dx *= scale;
                mapped_dy *= scale;
            }
            if k_distortion.abs() > 1e-5 {
                let r2_norm = (mapped_dx * mapped_dx + mapped_dy * mapped_dy) * max_radius_sq_inv;
                let f = 1.0 + k_distortion * r2_norm;
                mapped_dx *= f;
                mapped_dy *= f;
            }
            let mapped_ru = (mapped_dx * mapped_dx + mapped_dy * mapped_dy).sqrt();
            let scale = mapped_ru / ru;
            if scale > max_scale {
                max_scale = scale;
            }
        }
        if max_scale > 1.0 {
            max_scale * 1.002
        } else {
            max_scale
        }
    }

    fn sample(&self, src: &LinearImage, x: f32, y: f32) -> [f32; 3] {
        let src_raw = src.rgb();
        let sample_channel = |target_x: f32, target_y: f32, channel: usize| -> f32 {
            if target_x.is_nan() || target_y.is_nan() {
                return 0.0;
            }
            let x_clamped = target_x.clamp(0.0, self.width as f32 - 1.0);
            let y_clamped = target_y.clamp(0.0, self.height as f32 - 1.0);
            let mut x0 = x_clamped.floor() as usize;
            let mut y0 = y_clamped.floor() as usize;
            if x0 >= self.width - 1 {
                x0 = self.width.saturating_sub(2);
            }
            if y0 >= self.height - 1 {
                y0 = self.height.saturating_sub(2);
            }
            let wx = x_clamped - x0 as f32;
            let wy = y_clamped - y0 as f32;
            let idx = (y0 * self.width + x0) * 3 + channel;
            let p00 = src_raw[idx];
            let p10 = src_raw[idx + 3];
            let p01 = src_raw[idx + self.width * 3];
            let p11 = src_raw[idx + self.width * 3 + 3];
            let top = p00 * (1.0 - wx) + p10 * wx;
            let bot = p01 * (1.0 - wx) + p11 * wx;
            top * (1.0 - wy) + bot * wy
        };
        if self.has_tca {
            let rx = self.cx + (x - self.cx) * self.vr;
            let ry = self.cy + (y - self.cy) * self.vr;
            let bx = self.cx + (x - self.cx) * self.vb;
            let by = self.cy + (y - self.cy) * self.vb;
            [
                sample_channel(rx, ry, 0),
                sample_channel(x, y, 1),
                sample_channel(bx, by, 2),
            ]
        } else {
            // Plain interpolate_pixel: out-of-range leaves the pixel black.
            if x.is_nan()
                || y.is_nan()
                || x < 0.0
                || y < 0.0
                || x >= self.width as f32 - 1.0
                || y >= self.height as f32 - 1.0
            {
                return [0.0; 3];
            }
            let x0 = x.floor() as usize;
            let y0 = y.floor() as usize;
            let wx = x - x0 as f32;
            let wy = y - y0 as f32;
            let idx = (y0 * self.width + x0) * 3;
            let mut out = [0.0f32; 3];
            for c in 0..3 {
                let p00 = src_raw[idx + c];
                let p10 = src_raw[idx + 3 + c];
                let p01 = src_raw[idx + self.width * 3 + c];
                let p11 = src_raw[idx + self.width * 3 + 3 + c];
                let top = p00 * (1.0 - wx) + p10 * wx;
                let bot = p01 * (1.0 - wx) + p11 * wx;
                out[c] = top * (1.0 - wy) + bot * wy;
            }
            out
        }
    }

    fn warp(&self, src: &LinearImage) -> LinearImage {
        let (width, height) = src.dimensions();
        let mut out = LinearImage::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let mut src_x = x as f32;
                let mut src_y = y as f32;
                if self.auto_crop_scale > 1.0 {
                    src_x = self.cx + (src_x - self.cx) / self.auto_crop_scale;
                    src_y = self.cy + (src_y - self.cy) / self.auto_crop_scale;
                }
                if self.has_lens_correction {
                    let dx = (src_x - self.cx) as f64;
                    let dy = (src_y - self.cy) as f64;
                    let ru = (dx * dx + dy * dy).sqrt();
                    if ru > 1e-6 {
                        let ru_norm = ru / self.hd;
                        let ru_norm2 = ru_norm * ru_norm;
                        let rd_norm = if self.is_ptlens {
                            let (a, b, c) = (self.lk[0], self.lk[1], self.lk[2]);
                            let d = 1.0 - a - b - c;
                            ru_norm * (a * ru_norm2 * ru_norm + b * ru_norm2 + c * ru_norm + d)
                        } else {
                            ru_norm
                                * (1.0
                                    + self.lk[0] * ru_norm2
                                    + self.lk[1] * (ru_norm2 * ru_norm2)
                                    + self.lk[2] * (ru_norm2 * ru_norm2 * ru_norm2))
                        };
                        let effective_r_norm = ru_norm + (rd_norm - ru_norm) * self.lens_dist_amt;
                        let scale = effective_r_norm / ru_norm;
                        src_x = self.cx + (dx * scale) as f32;
                        src_y = self.cy + (dy * scale) as f32;
                    }
                }
                if self.k_distortion.abs() > 1e-5 {
                    let dx = (src_x - self.cx) as f64;
                    let dy = (src_y - self.cy) as f64;
                    let r2_norm = (dx * dx + dy * dy) * self.max_radius_sq_inv;
                    let f = 1.0 + self.k_distortion * r2_norm;
                    src_x = self.cx + (dx * f) as f32;
                    src_y = self.cy + (dy * f) as f32;
                }
                let mut pixel = self.sample(src, src_x, src_y);
                if self.has_vignetting {
                    let dx = (src_x - self.cx) as f64;
                    let dy = (src_y - self.cy) as f64;
                    let ru = (dx * dx + dy * dy).sqrt();
                    let ru_norm = ru / self.hd;
                    let ru_norm2 = ru_norm * ru_norm;
                    let v_factor = 1.0
                        + self.vk[0] * ru_norm2
                        + self.vk[1] * (ru_norm2 * ru_norm2)
                        + self.vk[2] * (ru_norm2 * ru_norm2 * ru_norm2);
                    if v_factor > 1e-6 {
                        let correction_gain = 1.0 / v_factor;
                        let final_gain = 1.0 + (correction_gain - 1.0) * self.lens_vig_amt;
                        pixel[0] *= final_gain as f32;
                        pixel[1] *= final_gain as f32;
                        pixel[2] *= final_gain as f32;
                    }
                }
                out.set_pixel(x, y, pixel);
            }
        }
        out
    }
}

fn gradient_image(width: u32, height: u32) -> LinearImage {
    LinearImage::from_fn(width, height, |x, y| {
        [
            (x as f32) / width as f32,
            (y as f32) / height as f32,
            ((x + y) as f32) / (width + height) as f32,
        ]
    })
}

fn assert_images_identical(a: &LinearImage, b: &LinearImage, context: &str) {
    assert_eq!(a.dimensions(), b.dimensions(), "{context}");
    for y in 0..a.height() {
        for x in 0..a.width() {
            let pa = a.pixel(x, y);
            let pb = b.pixel(x, y);
            assert!(
                pa[0].to_bits() == pb[0].to_bits()
                    && pa[1].to_bits() == pb[1].to_bits()
                    && pa[2].to_bits() == pb[2].to_bits(),
                "{context}: pixel ({x},{y}) differs: {pa:?} vs {pb:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Warp parity
// ---------------------------------------------------------------------------

#[test]
fn neutral_lens_warp_is_bit_identical() {
    let image = gradient_image(23, 11);
    let params = LensWarpParams::from_recipe(&rapidraw_edit_model::Recipe::default());
    let warped = lens_warp(&image, &params);
    assert_images_identical(
        &image,
        &warped,
        "neutral lens params must not change pixels",
    );
}

#[test]
fn poly3_distortion_matches_reference_formula() {
    let image = gradient_image(37, 23);
    let params = LensWarpParams {
        distortion: 0.0,
        lens_dist_k1: -0.05,
        lens_dist_k2: 0.02,
        lens_dist_k3: -0.005,
        lens_model: 0,
        lens_distortion_amount: 1.0,
        lens_distortion_enabled: true,
        tca_vr: 1.0,
        tca_vb: 1.0,
        lens_tca_amount: 1.0,
        lens_tca_enabled: true,
        vig_k1: 0.0,
        vig_k2: 0.0,
        vig_k3: 0.0,
        lens_vignette_amount: 1.0,
        lens_vignette_enabled: true,
    };
    let expected = RefWarp::new(37, 23, &params).warp(&image);
    let actual = lens_warp(&image, &params);
    assert_images_identical(&expected, &actual, "poly3 distortion parity");
}

#[test]
fn ptlens_distortion_with_manual_distortion_matches_reference_formula() {
    let image = gradient_image(41, 29);
    let params = LensWarpParams {
        distortion: 20.0,
        lens_dist_k1: 0.02,
        lens_dist_k2: -0.03,
        lens_dist_k3: 0.01,
        lens_model: 1,
        lens_distortion_amount: 1.5,
        lens_distortion_enabled: true,
        tca_vr: 1.0,
        tca_vb: 1.0,
        lens_tca_amount: 1.0,
        lens_tca_enabled: true,
        vig_k1: 0.0,
        vig_k2: 0.0,
        vig_k3: 0.0,
        lens_vignette_amount: 1.0,
        lens_vignette_enabled: true,
    };
    let expected = RefWarp::new(41, 29, &params).warp(&image);
    let actual = lens_warp(&image, &params);
    assert_images_identical(&expected, &actual, "ptlens + manual distortion parity");
}

#[test]
fn tca_and_vignetting_match_reference_formula() {
    let image = gradient_image(31, 19);
    let params = LensWarpParams {
        distortion: 0.0,
        lens_dist_k1: 0.0,
        lens_dist_k2: 0.0,
        lens_dist_k3: 0.0,
        lens_model: 0,
        lens_distortion_amount: 1.0,
        lens_distortion_enabled: true,
        tca_vr: 1.012,
        tca_vb: 0.991,
        lens_tca_amount: 1.0,
        lens_tca_enabled: true,
        vig_k1: -0.15,
        vig_k2: 0.04,
        vig_k3: -0.006,
        lens_vignette_amount: 1.0,
        lens_vignette_enabled: true,
    };
    let expected = RefWarp::new(31, 19, &params).warp(&image);
    let actual = lens_warp(&image, &params);
    assert_images_identical(&expected, &actual, "tca + vignetting parity");
}

#[test]
fn disabled_components_are_identity_terms() {
    let image = gradient_image(19, 13);
    let params = LensWarpParams {
        distortion: 30.0,
        lens_dist_k1: -0.04,
        lens_dist_k2: 0.0,
        lens_dist_k3: 0.0,
        lens_model: 0,
        lens_distortion_amount: 1.0,
        lens_distortion_enabled: false,
        tca_vr: 1.03,
        tca_vb: 0.97,
        lens_tca_amount: 1.0,
        lens_tca_enabled: false,
        vig_k1: -0.2,
        vig_k2: 0.0,
        vig_k3: 0.0,
        lens_vignette_amount: 1.0,
        lens_vignette_enabled: false,
    };
    // Manual distortion stays active (geometry slider), lens terms disabled.
    let expected = RefWarp::new(19, 13, &params).warp(&image);
    let actual = lens_warp(&image, &params);
    assert_images_identical(&expected, &actual, "disabled lens terms parity");
}

#[test]
fn auto_crop_scale_matches_reference_sample_points() {
    let params = LensWarpParams {
        distortion: 15.0,
        lens_dist_k1: -0.06,
        lens_dist_k2: 0.01,
        lens_dist_k3: 0.002,
        lens_model: 0,
        lens_distortion_amount: 1.2,
        lens_distortion_enabled: true,
        tca_vr: 1.0,
        tca_vb: 1.0,
        lens_tca_amount: 1.0,
        lens_tca_enabled: true,
        vig_k1: 0.0,
        vig_k2: 0.0,
        vig_k3: 0.0,
        lens_vignette_amount: 1.0,
        lens_vignette_enabled: true,
    };
    let expected = RefWarp::new(64, 48, &params).auto_crop_scale();
    let actual = lens_auto_crop_scale(64, 48, &params);
    assert!(
        (expected - actual).abs() < 1e-12,
        "auto crop scale {actual} vs reference {expected}"
    );
}

// ---------------------------------------------------------------------------
// Profile database: parsing, matching, resolution
// ---------------------------------------------------------------------------

const LENSFUN_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<lensdatabase>
    <camera>
        <maker>TestCorp</maker>
        <model>TestBody X1</model>
        <mount>TestMount</mount>
        <cropfactor>1.5</cropfactor>
    </camera>
    <lens>
        <maker>TestCorp</maker>
        <maker lang="en">TestCorp Optics</maker>
        <model>Test 24-70mm f/2.8</model>
        <mount>TestMount</mount>
        <cropfactor>1.5</cropfactor>
        <calibration>
            <distortion model="poly3" focal="24" k1="-0.01" k2="0.005" k3="0.001"/>
            <distortion model="poly3" focal="50" k1="-0.02" k2="0.01" k3="0.002"/>
            <tca model="linear" focal="24" vr="1.0004" vb="0.9998"/>
            <vignetting model="pa" focal="24" aperture="2.8" distance="10" k1="-0.2" k2="0.05" k3="0.01"/>
            <vignetting model="pa" focal="24" aperture="8" distance="10" k1="-0.05" k2="0.01" k3="0.0"/>
            <vignetting model="pa" focal="50" aperture="2.8" distance="10" k1="-0.3" k2="0.1" k3="0.02"/>
        </calibration>
    </lens>
    <lens>
        <maker>OtherCorp</maker>
        <model>Other 35mm f/1.4</model>
        <mount>TestMount</mount>
        <cropfactor>1.0</cropfactor>
        <calibration>
            <distortion model="ptlens" focal="35" a="0.012" b="-0.021" c="0.008"/>
        </calibration>
    </lens>
    <lens>
        <maker>LegacyCorp</maker>
        <model>Legacy 50mm f/2</model>
        <mount>LegacyMount</mount>
        <calibration>
            <distortion model="ptbrown" focal="50" k1="-0.01" k2="0.002"/>
        </calibration>
    </lens>
</lensdatabase>
"#;

fn db() -> LensDatabase {
    parse_lensfun_db(LENSFUN_XML).expect("fixture XML parses")
}

#[test]
fn parses_lensfun_xml_names_and_calibration() {
    let database = db();
    assert_eq!(database.cameras.len(), 1);
    assert_eq!(database.cameras[0].cropfactor, 1.5);
    assert_eq!(database.lenses.len(), 3);

    let zoom = database
        .lenses
        .iter()
        .find(|l| l.get_canonical_model_name() == "Test 24-70mm f/2.8")
        .expect("zoom lens present");
    // Reference semantics: the English maker/model names win for display.
    assert_eq!(zoom.get_maker(), "TestCorp Optics");
    assert_eq!(zoom.get_full_model_name(), "Test 24-70mm f/2.8");
    assert_eq!(zoom.get_name(), "Test 24-70mm f/2.8");
    assert_eq!(zoom.cropfactor, Some(1.5));
}

#[test]
fn resolve_matches_reference_interpolation_semantics() {
    let database = db();
    let maker = "TestCorp Optics"; // `get_maker()` prefers the English name.

    // Exact focal match.
    let (params, _) = resolve_lens_params(&database, maker, "Test 24-70mm f/2.8", 50.0, None, None)
        .expect("lens resolves");
    assert_eq!(params.model, 0.0);
    assert!((params.k1 - (-0.02)).abs() < 1e-6);
    assert!((params.k2 - 0.01).abs() < 1e-6);
    assert!((params.k3 - 0.002).abs() < 1e-6);

    // Mid focal interpolation: t = (37-24)/26 = 0.5.
    let (params, _) = resolve_lens_params(&database, maker, "Test 24-70mm f/2.8", 37.0, None, None)
        .expect("lens resolves");
    assert!((params.k1 - (-0.015)).abs() < 1e-6);
    assert!((params.k2 - 0.0075).abs() < 1e-6);
    assert!((params.k3 - 0.0015).abs() < 1e-6);

    // Below the lowest calibration focal clamps to the first entry.
    let (params, _) = resolve_lens_params(&database, maker, "Test 24-70mm f/2.8", 18.0, None, None)
        .expect("lens resolves");
    assert!((params.k1 - (-0.01)).abs() < 1e-6);

    // ptlens mapping: model flag 1, (a, b, c) in k1..k3.
    let (params, _) =
        resolve_lens_params(&database, "OtherCorp", "Other 35mm f/1.4", 35.0, None, None)
            .expect("lens resolves");
    assert_eq!(params.model, 1.0);
    assert!((params.k1 - 0.012).abs() < 1e-6);
    assert!((params.k2 - (-0.021)).abs() < 1e-6);
    assert!((params.k3 - 0.008).abs() < 1e-6);

    // Vignetting selects the closest aperture then the closest distance.
    let (params, _) = resolve_lens_params(
        &database,
        maker,
        "Test 24-70mm f/2.8",
        24.0,
        Some(8.0),
        None,
    )
    .expect("lens resolves");
    assert!((params.vig_k1 - (-0.05)).abs() < 1e-6);
    let (params, _) = resolve_lens_params(
        &database,
        maker,
        "Test 24-70mm f/2.8",
        24.0,
        Some(2.8),
        None,
    )
    .expect("lens resolves");
    assert!((params.vig_k1 - (-0.2)).abs() < 1e-6);

    // TCA linear interpolation across focals happens like distortion when
    // several tca entries exist; a single entry clamps to it.
    assert!((params.tca_vr - 1.0004).abs() < 1e-6);
}

#[test]
fn unsupported_distortion_model_is_an_explicit_error() {
    let database = db();
    let err = resolve_lens_params(&database, "LegacyCorp", "Legacy 50mm f/2", 50.0, None, None)
        .expect_err("unsupported distortion models must not resolve silently to zeros");
    match err {
        LensError::UnsupportedDistortionModel {
            maker,
            model,
            distortion_model,
            ..
        } => {
            assert_eq!(maker, "LegacyCorp");
            assert_eq!(model, "Legacy 50mm f/2");
            assert_eq!(distortion_model, "ptbrown");
        }
        other => panic!("expected UnsupportedDistortionModel, got {other:?}"),
    }
}

#[test]
fn missing_lens_is_profile_not_found() {
    let database = db();
    let err = resolve_lens_params(
        &database,
        "TestCorp Optics",
        "Nonexistent 8mm f/4",
        8.0,
        None,
        None,
    )
    .expect_err("unknown lenses must be explicit");
    assert!(
        matches!(err, LensError::ProfileNotFound { ref maker, ref model }
            if maker == "TestCorp Optics" && model == "Nonexistent 8mm f/4"),
        "expected ProfileNotFound, got {err:?}"
    );
}

#[test]
fn find_best_lens_match_returns_maker_and_display_name() {
    let database = db();
    // A camera-reported maker that does not equal `get_maker()` (the English
    // name) falls through to the reference's any-maker fuzzy fallback.
    let matched = find_best_lens_match(&database, "testcorp", "Test 24-70mm f/2.8")
        .expect("exact-ish match finds the lens");
    assert_eq!(matched.0, "TestCorp Optics");
    assert!(matched.1.contains("24-70mm"), "display name: {}", matched.1);

    assert!(find_best_lens_match(&database, "TestCorp", "zzz-nothing").is_none());
}

#[test]
fn poly3_tca_applies_the_linear_term_and_reports_partial_support() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<lensdatabase>
    <lens>
        <maker>PolyCorp</maker>
        <model>Poly 28mm f/2</model>
        <mount>M</mount>
        <calibration>
            <distortion model="poly3" focal="28" k1="-0.01" k2="0.0" k3="0.0"/>
            <tca model="poly3" focal="28" vr="1.0004" vb="0.9997" cr="0.00001" cb="-0.00001" br="0.00002" bb="-0.00003"/>
        </calibration>
    </lens>
</lensdatabase>
"#;
    let database = parse_lensfun_db(xml).expect("parses");
    let (params, limitations) =
        resolve_lens_params(&database, "PolyCorp", "Poly 28mm f/2", 28.0, None, None)
            .expect("distortion resolves");
    // Reference behavior: the linear vr/vb term of a poly3 TCA entry IS
    // applied; the cubic coefficients are ignored. That partial support is
    // surfaced as a visible capability notice.
    assert!((params.tca_vr - 1.0004).abs() < 1e-6);
    assert!((params.tca_vb - 0.9997).abs() < 1e-6);
    assert!(
        limitations.iter().any(|notice| {
            notice.kind == "partial-tca-model" && notice.detail.contains("poly3")
        }),
        "limitations must name the partially supported tca model: {limitations:?}"
    );
}

#[test]
fn unknown_tca_model_reports_a_limitation_without_changing_reference_output() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<lensdatabase>
    <lens>
        <maker>RhoCorp</maker>
        <model>Rho 28mm f/2</model>
        <mount>M</mount>
        <calibration>
            <distortion model="poly3" focal="28" k1="-0.01" k2="0.0" k3="0.0"/>
            <tca model="rho" focal="28" cr="0.00001" cb="-0.00001"/>
        </calibration>
    </lens>
</lensdatabase>
"#;
    let database = parse_lensfun_db(xml).expect("parses");
    let (params, limitations) =
        resolve_lens_params(&database, "RhoCorp", "Rho 28mm f/2", 28.0, None, None)
            .expect("distortion resolves");
    // Reference behavior: models without a vr/vb linear term resolve to
    // neutral (no TCA correction). The engine keeps that output but makes the
    // limitation visible instead of implicit.
    assert!((params.tca_vr - 1.0).abs() < 1e-6);
    assert!((params.tca_vb - 1.0).abs() < 1e-6);
    assert!(
        limitations.iter().any(|notice| {
            notice.kind == "unsupported-tca-model" && notice.detail.contains("rho")
        }),
        "limitations must name the unsupported tca model: {limitations:?}"
    );
}
