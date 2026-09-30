//! Oriented geometry coordinates and conversions from legacy pixel crops.
//!
//! # Oriented coordinate system
//!
//! All persisted geometry uses the **oriented, normalized** frame:
//!
//! - The oriented image is the source after applying `orientation_steps`
//!   clockwise quarter turns (0..=3) and the flip flags; `orientation_steps`
//!   of 1 or 3 swap the source width/height, exactly like the frontend's
//!   `getOrientedDimensions`.
//! - The origin is the top-left corner of the **oriented** image, x grows
//!   right, y grows down (the frame the user sees and crops in).
//! - Lengths are normalized to the oriented full size: `1.0` equals the full
//!   oriented width or height. A crop is `x, y, width, height` with all
//!   components finite in [0, 1], `width > 0`, `height > 0`,
//!   `x + width <= 1`, `y + height <= 1`.
//!
//! Normalization keeps recipes portable across preview scales and exports.
//! The legacy RapidRAW payload instead stores a pixel crop
//! (`react-image-crop`, `unit: 'px'`) in this same oriented frame, so the
//! conversion below is a pure scale — no rotation is applied here.
//!
//! # Persisted vs transient
//!
//! Crop, rotation, flips and aspect ratio are persisted render data.
//! Marquee selection, drag state, on-screen zoom and clipping overlays are
//! transient interaction state and have no representation in this crate.

use crate::ModelError;
use crate::types::CropRect;

/// Legacy pixel crop (`react-image-crop`, `unit: 'px'`) in oriented pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LegacyPixelCrop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Convert a legacy oriented pixel crop to the normalized recipe rectangle.
pub fn crop_to_normalized(
    legacy: LegacyPixelCrop,
    oriented_width: f64,
    oriented_height: f64,
) -> Result<CropRect, ModelError> {
    if !(oriented_width.is_finite() && oriented_width > 0.0)
        || !(oriented_height.is_finite() && oriented_height > 0.0)
    {
        return Err(ModelError::Validation(format!(
            "invalid oriented dimensions {oriented_width}x{oriented_height}"
        )));
    }
    for (name, v) in [
        ("x", legacy.x),
        ("y", legacy.y),
        ("width", legacy.width),
        ("height", legacy.height),
    ] {
        if !v.is_finite() {
            return Err(ModelError::Validation(format!(
                "legacy crop field {name} is not finite"
            )));
        }
    }
    if legacy.width <= 0.0 || legacy.height <= 0.0 {
        return Err(ModelError::Validation(
            "legacy crop width/height must be positive".to_string(),
        ));
    }
    let crop = CropRect {
        x: legacy.x / oriented_width,
        y: legacy.y / oriented_height,
        width: legacy.width / oriented_width,
        height: legacy.height / oriented_height,
    };
    validate_crop(&crop)?;
    Ok(crop)
}

/// Convert a normalized recipe rectangle back to legacy oriented pixels.
pub fn crop_to_legacy(
    crop: CropRect,
    oriented_width: f64,
    oriented_height: f64,
) -> Result<LegacyPixelCrop, ModelError> {
    validate_crop(&crop)?;
    Ok(LegacyPixelCrop {
        x: crop.x * oriented_width,
        y: crop.y * oriented_height,
        width: crop.width * oriented_width,
        height: crop.height * oriented_height,
    })
}

/// Shared crop rectangle bounds; used by validation too.
pub(crate) fn validate_crop(crop: &CropRect) -> Result<(), ModelError> {
    for (name, v) in [
        ("x", crop.x),
        ("y", crop.y),
        ("width", crop.width),
        ("height", crop.height),
    ] {
        if !v.is_finite() {
            return Err(ModelError::Validation(format!("crop.{name} is not finite")));
        }
    }
    if crop.width <= 0.0 || crop.height <= 0.0 {
        return Err(ModelError::Validation(
            "crop.width/height must be > 0".to_string(),
        ));
    }
    if crop.x < 0.0 || crop.y < 0.0 || crop.x + crop.width > 1.0 || crop.y + crop.height > 1.0 {
        return Err(ModelError::Validation(format!(
            "crop {:?} exceeds the oriented [0,1]x[0,1] frame",
            (crop.x, crop.y, crop.width, crop.height)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_pixel_frame() {
        let legacy = LegacyPixelCrop {
            x: 120.0,
            y: 60.0,
            width: 600.0,
            height: 300.0,
        };
        let normalized = crop_to_normalized(legacy, 1200.0, 600.0).unwrap();
        assert_eq!(
            normalized,
            CropRect {
                x: 0.1,
                y: 0.1,
                width: 0.5,
                height: 0.5
            }
        );
        let back = crop_to_legacy(normalized, 1200.0, 600.0).unwrap();
        assert_eq!(back, legacy);
    }

    #[test]
    fn rejects_out_of_frame_crops() {
        let crop = crop_to_normalized(
            LegacyPixelCrop {
                x: 1100.0,
                y: 0.0,
                width: 400.0,
                height: 600.0,
            },
            1200.0,
            600.0,
        );
        assert!(crop.is_err());
    }
}
