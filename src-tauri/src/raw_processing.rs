//! Typed host adapter over the extracted `rapidraw-develop` engine crate
//! (lap-517 / TASK-205, engine issue rapidraw-d73).
//!
//! The RAW decode pipeline itself lives in `rapidraw_develop::decode`; this
//! module only maps the host's historic public surface onto the engine's
//! typed inputs and back: untyped linear-mode strings, legacy
//! `(Arc<AtomicUsize>, usize)` cancellation pairs, `anyhow::Result`, and an
//! oriented `DynamicImage` output. Output stays byte-identical to the
//! pre-extraction host pipeline (pinned by the Lap RAW corpus baselines and
//! the `develop_decoder_parity`/`engine_client_routing` suites), and the
//! historic "Load cancelled" error text the loader classifies on is kept.

use anyhow::{Result, anyhow};
use image::{DynamicImage, ImageBuffer, Rgba};
use rapidraw_develop::{CancelToken, DecodeOptions, DevelopError, LinearRawMode, decode_original};
use rawler::{decoders::RawDecodeParams, rawsource::RawSource};
use std::sync::{Arc, atomic::AtomicUsize};

/// Decode RAW bytes through the engine crate, applying the metadata
/// orientation exactly like the host always has.
pub fn develop_raw_image(
    file_bytes: &[u8],
    fast_demosaic: bool,
    highlight_compression: f32,
    linear_mode: String,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<DynamicImage> {
    Ok(develop_raw_image_reported(
        file_bytes,
        fast_demosaic,
        highlight_compression,
        linear_mode,
        cancel_token,
    )?
    .0)
}

/// Engine-backed decode that additionally returns the engine's decode report
/// (source/output dimensions, detected orientation, linear-RAW flag,
/// white-balance neutralization, and the effective decode inputs).
pub fn develop_raw_image_reported(
    file_bytes: &[u8],
    fast_demosaic: bool,
    highlight_compression: f32,
    linear_mode: String,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<(DynamicImage, rapidraw_develop::DecodeReport)> {
    let cancellation = cancel_token
        .map(|(tracker, generation)| CancelToken::from_shared_parts(tracker, generation));
    let options = DecodeOptions {
        fast_demosaic,
        highlight_compression,
        linear_mode: linear_mode_from_host(&linear_mode),
        cancellation,
        ..DecodeOptions::default()
    };
    let decoded = decode_original(file_bytes, &options).map_err(map_engine_error)?;
    let (width, height) = decoded.image.dimensions();
    let buffer = ImageBuffer::<Rgba<f32>, _>::from_raw(width, height, decoded.image.rgba32f())
        .expect("engine returned a full-width linear buffer");
    Ok((DynamicImage::ImageRgba32F(buffer), decoded.report))
}

/// Map the host's untyped linear-mode setting onto the engine's enum with the
/// exact fallback of the pre-extraction host implementation (everything
/// unknown decodes as auto).
fn linear_mode_from_host(linear_mode: &str) -> LinearRawMode {
    match linear_mode {
        "gamma" => LinearRawMode::Gamma,
        "skip_calib" => LinearRawMode::SkipCalib,
        "gamma_skip_calib" => LinearRawMode::GammaSkipCalib,
        _ => LinearRawMode::Auto,
    }
}

/// Preserve the host's public error semantics: the loader classifies
/// cancellations by the literal "Load cancelled" text; every other engine
/// failure stays an explicit, visible decode error.
fn map_engine_error(error: DevelopError) -> anyhow::Error {
    match error {
        DevelopError::Cancelled => anyhow!("Load cancelled"),
        other => anyhow!("Failed to decode RAW: {other}"),
    }
}

pub fn get_fast_demosaic_scale_factor(
    file_bytes: &[u8],
    decoded_width: u32,
    decoded_height: u32,
) -> f32 {
    let source = RawSource::new_from_slice(file_bytes);
    if let Ok(decoder) = rawler::get_decoder(&source)
        && let Ok(raw_img) = decoder.raw_image(&source, &RawDecodeParams::default(), true)
    {
        let max_orig = (raw_img.width as f32).max(raw_img.height as f32);
        let max_comp = (decoded_width as f32).max(decoded_height as f32);
        if max_orig > 0.0 {
            let ratio = max_comp / max_orig;
            if ratio > 0.1 && ratio < 0.35 {
                return 0.25;
            } else if (0.35..0.75).contains(&ratio) {
                return 0.5;
            }
        }
    }
    1.0
}
