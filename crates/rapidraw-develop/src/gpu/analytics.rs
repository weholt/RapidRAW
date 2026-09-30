//! Renderer-derived analytics contracts. The histogram is computed from the
//! actual rendered pixels produced by the GPU pipeline (the readback of the
//! processing shader's RGBA8 output), so the bins always correspond to the
//! rendered state — the host can never apply adjustments a second time on
//! top of the histogram.

use rayon::prelude::*;
use serde::Serialize;

/// Color-transform contract of the rendered pixels the GPU pipeline hands
/// back. `Rgba8Srgb` means: 8-bit RGBA, non-linear sRGB-encoded as produced
/// by the processing shader's output write (the same encoding the host
/// displays and encodes for export). Histogram bins from
/// [`calculate_histogram_from_image`] index exactly this encoding; no
/// second color transform is applied by the analytics themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputColorSpace {
    /// Non-linear sRGB-encoded RGBA8 readback of the rendered output.
    Rgba8Srgb,
}

/// Histogram over the rendered pixels: 256 bins per channel, Gaussian
/// smoothed (sigma 2.0) and 99th-percentile normalized to [0, 1], identical
/// to the pre-extraction host formula (`calculate_histogram_from_image` in
/// RapidRAW `image_processing.rs`), so existing UI consumers see the same
/// numbers.
#[derive(Serialize, Clone, Debug)]
pub struct HistogramBins {
    pub red: Vec<f32>,
    pub green: Vec<f32>,
    pub blue: Vec<f32>,
    pub luma: Vec<f32>,
}

/// Sampled-bin accumulator for 8-bit RGB pixels (every second pixel, luma
/// via the host's integer approximation `(r*218 + g*732 + b*74) >> 10`).
fn accumulate_rgb8_bins(raw: &[u8]) -> ([u32; 256], [u32; 256], [u32; 256], [u32; 256]) {
    let mut acc = ([0u32; 256], [0u32; 256], [0u32; 256], [0u32; 256]);
    for pixel in raw.as_chunks::<3>().0.iter().step_by(2) {
        let r = pixel[0] as usize;
        let g = pixel[1] as usize;
        let b = pixel[2] as usize;

        acc.0[r] += 1;
        acc.1[g] += 1;
        acc.2[b] += 1;

        let luma = (r * 218 + g * 732 + b * 74) >> 10;
        acc.3[luma.min(255)] += 1;
    }
    acc
}

fn merge_bins(
    mut a: ([u32; 256], [u32; 256], [u32; 256], [u32; 256]),
    b: ([u32; 256], [u32; 256], [u32; 256], [u32; 256]),
) -> ([u32; 256], [u32; 256], [u32; 256], [u32; 256]) {
    for i in 0..256 {
        a.0[i] += b.0[i];
        a.1[i] += b.1[i];
        a.2[i] += b.2[i];
        a.3[i] += b.3[i];
    }
    a
}

/// Histogram of the rendered output. `color_space` must be
/// [`OutputColorSpace::Rgba8Srgb`] — the only encoding the GPU pipeline
/// produces. `pixels` is tightly packed RGBA8 with `width * height * 4`
/// bytes.
pub fn histogram_from_rendered(
    pixels: &[u8],
    width: u32,
    height: u32,
    color_space: OutputColorSpace,
) -> Result<HistogramBins, super::error::GpuError> {
    debug_assert_eq!(color_space, OutputColorSpace::Rgba8Srgb);
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| {
            super::error::GpuError::InvalidRequest("rendered pixel count overflow".to_string())
        })?;
    if pixels.len() < expected {
        return Err(super::error::GpuError::InvalidRequest(format!(
            "rendered pixel buffer too small: {} bytes for {}x{}",
            pixels.len(),
            width,
            height
        )));
    }

    let rgb: Vec<u8> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();

    Ok(finish_bins(rgb.as_slice()))
}

fn finish_bins(rgb8: &[u8]) -> HistogramBins {
    let (r_c, g_c, b_c, l_c) = rgb8
        .par_chunks(30_000)
        .fold(
            || ([0u32; 256], [0u32; 256], [0u32; 256], [0u32; 256]),
            |mut acc, chunk| {
                let part = accumulate_rgb8_bins(chunk);
                acc = merge_bins(acc, part);
                acc
            },
        )
        .reduce(
            || ([0u32; 256], [0u32; 256], [0u32; 256], [0u32; 256]),
            merge_bins,
        );

    let mut red: Vec<f32> = r_c.into_iter().map(|c| c as f32).collect();
    let mut green: Vec<f32> = g_c.into_iter().map(|c| c as f32).collect();
    let mut blue: Vec<f32> = b_c.into_iter().map(|c| c as f32).collect();
    let mut luma: Vec<f32> = l_c.into_iter().map(|c| c as f32).collect();

    let smoothing_sigma = 2.0;
    apply_gaussian_smoothing(&mut red, smoothing_sigma);
    apply_gaussian_smoothing(&mut green, smoothing_sigma);
    apply_gaussian_smoothing(&mut blue, smoothing_sigma);
    apply_gaussian_smoothing(&mut luma, smoothing_sigma);

    normalize_histogram_range(&mut red, 0.99);
    normalize_histogram_range(&mut green, 0.99);
    normalize_histogram_range(&mut blue, 0.99);
    normalize_histogram_range(&mut luma, 0.99);

    HistogramBins {
        red,
        green,
        blue,
        luma,
    }
}

/// Histogram of a decoded image (any `DynamicImage` layout). The RGB32F
/// path clamps and quantizes to the same 8-bit bins the rendered-output
/// path uses, so both entries of the contract produce directly comparable
/// bin vectors.
pub fn calculate_histogram_from_image(
    image: &image::DynamicImage,
) -> Result<HistogramBins, super::error::GpuError> {
    if let image::DynamicImage::ImageRgb32F(f32_img) = image {
        let raw = f32_img.as_raw();
        let rgb: Vec<u8> = raw
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| {
                [
                    (p[0].clamp(0.0, 1.0) * 255.0) as u8,
                    (p[1].clamp(0.0, 1.0) * 255.0) as u8,
                    (p[2].clamp(0.0, 1.0) * 255.0) as u8,
                ]
            })
            .collect();
        return Ok(finish_bins(&rgb));
    }
    let rgb = image.to_rgb8();
    Ok(finish_bins(rgb.as_raw()))
}

/// Gaussian smoothing of the bin vector, moved verbatim from the host.
pub fn apply_gaussian_smoothing(histogram: &mut [f32], sigma: f32) {
    if sigma <= 0.0 {
        return;
    }

    let kernel_radius = (sigma * 3.0).ceil() as usize;
    if kernel_radius == 0 || kernel_radius >= histogram.len() {
        return;
    }

    let kernel_size = 2 * kernel_radius + 1;
    let mut kernel = vec![0.0; kernel_size];
    let mut kernel_sum = 0.0;

    let two_sigma_sq = 2.0 * sigma * sigma;
    for (i, kernel_val) in kernel.iter_mut().enumerate() {
        let x = (i as i32 - kernel_radius as i32) as f32;
        let val = (-x * x / two_sigma_sq).exp();
        *kernel_val = val;
        kernel_sum += val;
    }

    if kernel_sum > 0.0 {
        for val in &mut kernel {
            *val /= kernel_sum;
        }
    }

    let original = histogram.to_owned();
    let len = histogram.len();

    for (i, hist_val) in histogram.iter_mut().enumerate() {
        let mut smoothed_val = 0.0;
        for (k, &kernel_val) in kernel.iter().enumerate() {
            let offset = k as i32 - kernel_radius as i32;
            let sample_index = i as i32 + offset;
            let clamped_index = sample_index.clamp(0, len as i32 - 1) as usize;
            smoothed_val += original[clamped_index] * kernel_val;
        }
        *hist_val = smoothed_val;
    }
}

/// 99th-percentile normalization of the bin vector, moved verbatim from the
/// host.
pub fn normalize_histogram_range(histogram: &mut [f32], percentile_clip: f32) {
    if histogram.is_empty() {
        return;
    }

    let mut sorted_data = histogram.to_owned();
    sorted_data.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let clip_index = ((sorted_data.len() - 1) as f32 * percentile_clip).round() as usize;
    let max_val = sorted_data[clip_index.min(sorted_data.len() - 1)];

    if max_val > 1e-6 {
        let scale_factor = 1.0 / max_val;
        for value in histogram.iter_mut() {
            *value = (*value * scale_factor).min(1.0);
        }
    } else {
        for value in histogram.iter_mut() {
            *value = 0.0;
        }
    }
}
