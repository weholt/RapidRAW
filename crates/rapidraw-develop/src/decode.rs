//! RAW decode: typed options, linear buffer ownership, explicit metadata.
//!
//! `decode_original` is a faithful extraction of the RapidRAW host's
//! `raw_processing::develop_internal` (revision
//! `5e30bcbb246395d391ba2e9662510641ffe68e6b`): rawler decode with the same
//! step retention, level rescaling, highlight-compression rolloff, and
//! linearization order, so extracted output matches the pre-extraction
//! fixtures pixel for pixel.

use crate::buffer::LinearImage;
use crate::error::DevelopError;
use crate::geometry::apply_orientation;
use crate::wb::{WhiteBalancePolicy, neutralize_wb_if_multiexposure};
use crate::{LinearRawMode, ToneMapper};
use rawler::decoders::{Orientation, RawDecodeParams};
use rawler::imgop::develop::{DemosaicAlgorithm, Intermediate, ProcessingStep, RawDevelop};
use rawler::rawimage::{RawImage, RawPhotometricInterpretation};
use rawler::rawsource::RawSource;

/// How the decode result relates to the RAW metadata orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrientationHandling {
    /// Apply the detected orientation to the returned image (the host's
    /// historic `develop_raw_image` behavior). The report still carries the
    /// detected orientation.
    #[default]
    ApplyFromMetadata,
    /// Return the image in the unoriented source frame; callers apply
    /// [`crate::geometry::apply_orientation`] themselves.
    KeepUnoriented,
}

/// Cancellation source: bump the generation to cancel outstanding decodes
/// holding a matching [`CancelToken`].
#[derive(Clone, Default)]
pub struct CancelSource(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl CancelSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel every [`CancelToken`] created from this source at its current
    /// generation.
    pub fn cancel(&self) {
        use std::sync::atomic::Ordering;
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn generation(&self) -> usize {
        use std::sync::atomic::Ordering;
        self.0.load(Ordering::SeqCst)
    }
}

/// A token bound to a [`CancelSource`] generation.
#[derive(Clone)]
pub struct CancelToken {
    source: CancelSource,
    generation: usize,
}

impl CancelToken {
    pub fn pair() -> (CancelSource, CancelToken) {
        let source = CancelSource::new();
        let generation = source.generation();
        (source.clone(), CancelToken { source, generation })
    }

    pub fn is_cancelled(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.source.0.load(Ordering::SeqCst) != self.generation
    }

    pub fn check(&self) -> Result<(), DevelopError> {
        if self.is_cancelled() {
            Err(DevelopError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Progress/metadata events emitted by the decoder. Policy and reporting are
/// separated from the pixel math: observers never influence the output.
pub enum DecodeEvent {
    DecoderReady { source_dimensions: (u32, u32) },
    OrientationDetected { orientation: Orientation },
    WhiteBalanceNeutralized,
    Completed { output_dimensions: (u32, u32) },
}

/// Observer callback receiving decode metadata events.
pub type DecodeObserver = dyn Fn(&DecodeEvent) + Send + Sync;

/// Typed decode inputs. Every effective global interpretation setting is an
/// explicit field; defaults reproduce the RapidRAW host behavior at the
/// extraction revision (highlight compression 2.5, linear mode auto,
/// auto-neutralize multi-exposure white balance, apply metadata orientation).
pub struct DecodeOptions {
    /// Fast (superpixel) demosaic; quarter-size output like the host's
    /// `use_fast_raw_dev` path.
    pub fast_demosaic: bool,
    /// Effective highlight compression (engine default 2.5, bounded below at
    /// 1.01 exactly like the host).
    pub highlight_compression: f32,
    /// Effective linear-RAW interpretation mode.
    pub linear_mode: LinearRawMode,
    /// Effective tone-mapper override. Recorded on the report; the rawler
    /// decode path does not tone-map (the render stage applies it), matching
    /// the pre-extraction pipeline.
    pub tone_mapper: Option<ToneMapper>,
    /// White-balance interpretation policy.
    pub white_balance: WhiteBalancePolicy,
    /// How the RAW orientation is applied to the returned image.
    pub orientation: OrientationHandling,
    /// Optional cancellation checkpoint token.
    pub cancellation: Option<CancelToken>,
    /// Optional metadata observer.
    pub observer: Option<Box<DecodeObserver>>,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            fast_demosaic: false,
            highlight_compression: 2.5,
            linear_mode: LinearRawMode::Auto,
            tone_mapper: None,
            white_balance: WhiteBalancePolicy::default(),
            orientation: OrientationHandling::default(),
            cancellation: None,
            observer: None,
        }
    }
}

impl DecodeOptions {
    /// Map the shared recipe model's effective decode settings onto decode
    /// options. Color noise reduction and sharpening are host-side
    /// preprocessing and intentionally have no decode counterpart.
    pub fn from_effective(effective: &rapidraw_edit_model::types::EffectiveDecodeSettings) -> Self {
        Self {
            fast_demosaic: effective.fast_demosaic,
            highlight_compression: effective.highlight_compression as f32,
            linear_mode: effective.linear_raw_mode,
            tone_mapper: effective.tonemapper_override,
            ..Self::default()
        }
    }
}

/// Metadata about a completed decode.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodeReport {
    /// Dimensions of the decoded source (before orientation).
    pub source_dimensions: (u32, u32),
    /// Dimensions of the returned image.
    pub output_dimensions: (u32, u32),
    /// Orientation detected in the RAW metadata.
    pub orientation: Orientation,
    /// The photometric interpretation was linear RAW.
    pub is_linear_raw: bool,
    /// The white-balance policy neutralized coefficients.
    pub wb_neutralized: bool,
    /// The effective inputs actually applied.
    pub fast_demosaic: bool,
    pub highlight_compression: f32,
    pub linear_mode: LinearRawMode,
    pub tone_mapper: Option<ToneMapper>,
}

/// A decoded RAW original: a linear, high-precision buffer at full decoded
/// dimensions plus the decode metadata. There is no preview representation
/// anywhere in this result.
#[derive(Debug, Clone)]
pub struct DecodedOriginal {
    pub image: LinearImage,
    pub report: DecodeReport,
}

#[inline]
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(3.0)
    }
}

fn is_linear_raw_format(raw_image: &RawImage) -> bool {
    matches!(
        raw_image.photometric,
        RawPhotometricInterpretation::LinearRaw
    )
}

/// Decode RAW bytes into a linear, high-precision original.
///
/// Embedded, downscaled, or 8-bit previews are never returned: the decoder
/// owns the full-dimension linear buffer, and any failure is an explicit
/// [`DevelopError`] (never a silent fallback).
pub fn decode_original(
    file_bytes: &[u8],
    options: &DecodeOptions,
) -> Result<DecodedOriginal, DevelopError> {
    if file_bytes.is_empty() {
        return Err(DevelopError::InvalidInput("RAW input is empty".to_string()));
    }

    let check_cancel = || -> Result<(), DevelopError> {
        if let Some(token) = &options.cancellation {
            token.check()?;
        }
        Ok(())
    };
    let emit = |event: &DecodeEvent| {
        if let Some(observer) = &options.observer {
            observer(event);
        }
    };

    check_cancel()?;

    let source = RawSource::new_from_slice(file_bytes);
    let decoder = rawler::get_decoder(&source).map_err(|e| DevelopError::Decode(e.to_string()))?;

    check_cancel()?;
    let mut raw_image: RawImage = decoder
        .raw_image(&source, &RawDecodeParams::default(), false)
        .map_err(|e| DevelopError::Decode(e.to_string()))?;

    let source_dimensions = (raw_image.width as u32, raw_image.height as u32);
    emit(&DecodeEvent::DecoderReady { source_dimensions });

    let metadata = decoder
        .raw_metadata(&source, &RawDecodeParams::default())
        .map_err(|e| DevelopError::Decode(e.to_string()))?;
    let orientation = metadata
        .exif
        .orientation
        .map(Orientation::from_u16)
        .unwrap_or(Orientation::Normal);
    emit(&DecodeEvent::OrientationDetected { orientation });

    let is_linear_format = is_linear_raw_format(&raw_image);

    let (apply_ungamma, apply_calibration) = match options.linear_mode {
        LinearRawMode::Gamma => (true, true),
        LinearRawMode::SkipCalib => (false, false),
        LinearRawMode::GammaSkipCalib => (true, false),
        LinearRawMode::Auto => (false, true),
    };

    let original_white_level = raw_image
        .whitelevel
        .0
        .first()
        .cloned()
        .unwrap_or(u16::MAX as u32) as f32;
    let original_black_level = raw_image
        .blacklevel
        .levels
        .first()
        .map(|r| r.as_f32())
        .unwrap_or(0.0);

    for level in raw_image.whitelevel.0.iter_mut() {
        *level = u32::MAX;
    }

    let mut developer = RawDevelop::default();

    if is_linear_format {
        developer.steps.retain(|&step| {
            step != ProcessingStep::SRgb
                && step != ProcessingStep::Demosaic
                && (apply_calibration || step != ProcessingStep::Calibrate)
        });
    } else if options.fast_demosaic {
        developer.demosaic_algorithm = DemosaicAlgorithm::Speed;
        developer.steps.retain(|&step| step != ProcessingStep::SRgb);
    } else {
        developer.steps.retain(|&step| step != ProcessingStep::SRgb);
    }

    let (wb_coeffs, wb_neutralized) =
        neutralize_wb_if_multiexposure(raw_image.wb_coeffs, file_bytes, options.white_balance);
    raw_image.wb_coeffs = wb_coeffs;
    if wb_neutralized {
        emit(&DecodeEvent::WhiteBalanceNeutralized);
    }

    check_cancel()?;
    let mut developed_intermediate = developer
        .develop_intermediate(&raw_image)
        .map_err(|e| DevelopError::Decode(e.to_string()))?;

    drop(raw_image);

    let denominator = (original_white_level - original_black_level).max(1.0);
    let rescale_factor = (u32::MAX as f32 - original_black_level) / denominator;

    let safe_highlight_compression = options.highlight_compression.max(1.01);

    let clamp_limit = if options.fast_demosaic {
        1.0
    } else {
        safe_highlight_compression
    };

    check_cancel()?;

    match &mut developed_intermediate {
        Intermediate::Monochrome(pixels) => {
            pixels.data.iter_mut().for_each(|p| {
                let mut linear_val = *p * rescale_factor;
                if is_linear_format && apply_ungamma {
                    linear_val = srgb_to_linear(linear_val.clamp(0.0, 1.0));
                }
                *p = linear_val.clamp(0.0, clamp_limit);
            });
        }
        Intermediate::ThreeColor(pixels) => {
            pixels.data.iter_mut().for_each(|p| {
                let mut r = (p[0] * rescale_factor).max(0.0);
                let mut g = (p[1] * rescale_factor).max(0.0);
                let mut b = (p[2] * rescale_factor).max(0.0);

                if is_linear_format && apply_ungamma {
                    r = srgb_to_linear(r.clamp(0.0, 1.0));
                    g = srgb_to_linear(g.clamp(0.0, 1.0));
                    b = srgb_to_linear(b.clamp(0.0, 1.0));
                }

                let max_c = r.max(g).max(b);

                let (final_r, final_g, final_b) = if max_c > 1.0 {
                    let min_c = r.min(g).min(b);
                    let compression_factor =
                        (1.0 - (max_c - 1.0) / (safe_highlight_compression - 1.0)).clamp(0.0, 1.0);
                    let compressed_r = min_c + (r - min_c) * compression_factor;
                    let compressed_g = min_c + (g - min_c) * compression_factor;
                    let compressed_b = min_c + (b - min_c) * compression_factor;
                    let compressed_max = compressed_r.max(compressed_g).max(compressed_b);

                    if compressed_max > 1e-6 {
                        let rescale = max_c / compressed_max;
                        (
                            compressed_r * rescale,
                            compressed_g * rescale,
                            compressed_b * rescale,
                        )
                    } else {
                        (max_c, max_c, max_c)
                    }
                } else {
                    (r, g, b)
                };

                p[0] = final_r.clamp(0.0, clamp_limit);
                p[1] = final_g.clamp(0.0, clamp_limit);
                p[2] = final_b.clamp(0.0, clamp_limit);
            });
        }
        Intermediate::FourColor(pixels) => {
            pixels.data.iter_mut().for_each(|p| {
                p.iter_mut().for_each(|c| {
                    let mut linear_val = *c * rescale_factor;
                    if is_linear_format && apply_ungamma {
                        linear_val = srgb_to_linear(linear_val.clamp(0.0, 1.0));
                    }
                    *c = linear_val.clamp(0.0, clamp_limit);
                });
            });
        }
    }

    let (width, height) = {
        let dim = developed_intermediate.dim();
        (dim.w as u32, dim.h as u32)
    };

    check_cancel()?;

    let image = match developed_intermediate {
        Intermediate::ThreeColor(pixels) => {
            let mut out = LinearImage::new(width, height);
            for y in 0..height {
                for x in 0..width {
                    let p = pixels.data[(y * width + x) as usize];
                    out.set_pixel(x, y, [p[0], p[1], p[2]]);
                }
            }
            out
        }
        Intermediate::Monochrome(pixels) => {
            let mut out = LinearImage::new(width, height);
            for y in 0..height {
                for x in 0..width {
                    let p = pixels.data[(y * width + x) as usize];
                    out.set_pixel(x, y, [p, p, p]);
                }
            }
            out
        }
        _ => {
            return Err(DevelopError::Unsupported(
                "unsupported intermediate format for conversion".to_string(),
            ));
        }
    };

    let oriented = match options.orientation {
        OrientationHandling::ApplyFromMetadata => apply_orientation(&image, orientation),
        OrientationHandling::KeepUnoriented => image,
    };

    let output_dimensions = oriented.dimensions();
    emit(&DecodeEvent::Completed { output_dimensions });

    Ok(DecodedOriginal {
        image: oriented,
        report: DecodeReport {
            source_dimensions,
            output_dimensions,
            orientation,
            is_linear_raw: is_linear_format,
            wb_neutralized,
            fast_demosaic: options.fast_demosaic,
            highlight_compression: safe_highlight_compression,
            linear_mode: options.linear_mode,
            tone_mapper: options.tone_mapper,
        },
    })
}
