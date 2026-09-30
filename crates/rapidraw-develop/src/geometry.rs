//! Oriented-frame geometry: orientation application, quarter-turn rotation,
//! flips, and crop with explicit coordinate conversions.
//!
//! # Oriented coordinate system
//!
//! RAW metadata carries an orientation (EXIF `Orientation` tag, values
//! 1..=8, mapped to rawler's [`Orientation`]). The **oriented** image is the
//! decoded source after applying that orientation, exactly like the host's
//! `apply_orientation` did before extraction:
//!
//! | `Orientation`     | Transform (image-crate equivalent)      |
//! | ----------------- | --------------------------------------- |
//! | `Normal`/`Unknown`| identity                                 |
//! | `HorizontalFlip`  | mirror left-right                        |
//! | `Rotate180`       | 180-degree turn                          |
//! | `VerticalFlip`    | mirror top-bottom                        |
//! | `Transpose`       | rotate 90 degrees clockwise, then mirror left-right |
//! | `Rotate90`        | rotate 90 degrees clockwise              |
//! | `Transverse`      | rotate 270 degrees clockwise, then mirror left-right |
//! | `Rotate270`       | rotate 270 degrees clockwise             |
//!
//! The origin is the top-left corner of the **oriented** image; x grows
//! right, y grows down. All persisted crops (the recipe's normalized
//! [`CropRect`]) live in this oriented frame. The quarter-turn orientations
//! swap width and height ([`oriented_dimensions`]).
//!
//! [`crop_to_source_rect`] converts an oriented, normalized crop back to a
//! pixel rectangle in the **unoriented source frame**, so callers can relate
//! persisted geometry to the raw decode buffer. Mappings are integer-exact:
//! a rectangle stays axis-aligned under 90-degree turns and mirror flips.

use crate::CropRect;
use crate::buffer::LinearImage;
use crate::error::DevelopError;
use rawler::decoders::Orientation;

/// A pixel rectangle with top-left origin and exclusive right/bottom edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Dimensions of the oriented image for a decoded source of `w x h`.
pub fn oriented_dimensions(
    source_width: u32,
    source_height: u32,
    orientation: Orientation,
) -> (u32, u32) {
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Transpose
        | Orientation::Transverse => (source_height, source_width),
        _ => (source_width, source_height),
    }
}

/// Map an oriented pixel coordinate back to its source pixel coordinate.
///
/// `(x, y)` must lie inside the oriented dimensions; `source_width` and
/// `source_height` describe the unoriented decoded image. Returns `None` when
/// the coordinate is outside the oriented frame.
pub fn oriented_to_source_pixel(
    x: u32,
    y: u32,
    source_width: u32,
    source_height: u32,
    orientation: Orientation,
) -> Option<(u32, u32)> {
    let (oriented_w, oriented_h) = oriented_dimensions(source_width, source_height, orientation);
    if x >= oriented_w || y >= oriented_h {
        return None;
    }
    let (w, h) = (source_width, source_height);
    let mapped = match orientation {
        Orientation::Normal | Orientation::Unknown => (x, y),
        Orientation::HorizontalFlip => (w - 1 - x, y),
        Orientation::Rotate180 => (w - 1 - x, h - 1 - y),
        Orientation::VerticalFlip => (x, h - 1 - y),
        // Forward map: source (sx, sy) -> oriented (h - 1 - sy, sx).
        Orientation::Rotate90 => (y, h - 1 - x),
        // Forward map: source (sx, sy) -> oriented (sy, w - 1 - sx).
        Orientation::Rotate270 => (w - 1 - y, x),
        // Forward map: source (sx, sy) -> oriented (sy, sx).
        Orientation::Transpose => (y, x),
        // Forward map: source (sx, sy) -> oriented (h - 1 - sy, w - 1 - sx).
        Orientation::Transverse => (w - 1 - y, h - 1 - x),
    };
    Some(mapped)
}

/// Apply the RAW metadata orientation exactly like the host's
/// `apply_orientation` helper.
pub fn apply_orientation(image: &LinearImage, orientation: Orientation) -> LinearImage {
    let (out_w, out_h) = oriented_dimensions(image.width(), image.height(), orientation);
    let mut out = LinearImage::new(out_w, out_h);
    for y in 0..out_h {
        for x in 0..out_w {
            let Some((sx, sy)) =
                oriented_to_source_pixel(x, y, image.width(), image.height(), orientation)
            else {
                continue;
            };
            out.set_pixel(x, y, image.pixel(sx, sy));
        }
    }
    out
}

/// Apply coarse UI rotation steps (0 = none, 1 = 90 degrees clockwise,
/// 2 = 180, 3 = 270), mirroring the host's `apply_coarse_rotation`.
pub fn apply_coarse_rotation(image: &LinearImage, orientation_steps: u8) -> LinearImage {
    match orientation_steps {
        1 => apply_orientation(image, Orientation::Rotate90),
        2 => apply_orientation(image, Orientation::Rotate180),
        3 => apply_orientation(image, Orientation::Rotate270),
        _ => image.clone(),
    }
}

/// Apply horizontal and/or vertical flips (horizontal first), mirroring the
/// host's `apply_flip`.
pub fn apply_flip(image: &LinearImage, horizontal: bool, vertical: bool) -> LinearImage {
    let mut out = image.clone();
    if horizontal {
        out = apply_orientation(&out, Orientation::HorizontalFlip);
    }
    if vertical {
        out = apply_orientation(&out, Orientation::VerticalFlip);
    }
    out
}

/// Crop by pixel coordinates with the host's clamping semantics: coordinates
/// are rounded (f64 -> u32), a crop that starts outside the image or has a
/// degenerate size returns the image unchanged, and the requested size is
/// clamped to the image bounds.
pub fn apply_pixel_crop(
    image: &LinearImage,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> LinearImage {
    let x = x.round() as u32;
    let y = y.round() as u32;
    let width = width.round() as u32;
    let height = height.round() as u32;

    if width > 0 && height > 0 {
        let (img_w, img_h) = image.dimensions();
        if x < img_w && y < img_h {
            let new_width = (img_w - x).min(width);
            let new_height = (img_h - y).min(height);

            if new_width > 0 && new_height > 0 {
                if x == 0 && y == 0 && new_width == img_w && new_height == img_h {
                    return image.clone();
                }
                let mut out = LinearImage::new(new_width, new_height);
                for row in 0..new_height {
                    let src = ((y + row) as usize * img_w as usize + x as usize) * 3;
                    let dst = row as usize * new_width as usize * 3;
                    let span = new_width as usize * 3;
                    out.rgb_mut()[dst..dst + span].copy_from_slice(&image.rgb()[src..src + span]);
                }
                return out;
            }
        }
    }
    image.clone()
}

fn validate_normalized_crop(crop: &CropRect) -> Result<(), DevelopError> {
    for (name, value) in [
        ("x", crop.x),
        ("y", crop.y),
        ("width", crop.width),
        ("height", crop.height),
    ] {
        if !value.is_finite() {
            return Err(DevelopError::Geometry(format!("crop {name} is not finite")));
        }
    }
    if crop.width <= 0.0 || crop.height <= 0.0 {
        return Err(DevelopError::Geometry(
            "crop width/height must be positive".to_string(),
        ));
    }
    if crop.x < 0.0 || crop.y < 0.0 || crop.x + crop.width > 1.0 || crop.y + crop.height > 1.0 {
        return Err(DevelopError::Geometry(
            "crop rectangle exceeds the oriented frame".to_string(),
        ));
    }
    Ok(())
}

/// Crop by a normalized [`CropRect`] in the oriented frame.
///
/// The rect components must be finite with `width > 0`, `height > 0`,
/// `x + width <= 1`, `y + height <= 1` (recipe contract from
/// `rapidraw-edit-model`). Pixel edges are derived by scaling with the
/// oriented dimensions and rounding, then cropping with the same clamping
/// semantics as [`apply_pixel_crop`].
pub fn apply_crop_normalized(
    image: &LinearImage,
    crop: &CropRect,
) -> Result<LinearImage, DevelopError> {
    validate_normalized_crop(crop)?;
    let (img_w, img_h) = image.dimensions();
    Ok(apply_pixel_crop(
        image,
        crop.x * f64::from(img_w),
        crop.y * f64::from(img_h),
        crop.width * f64::from(img_w),
        crop.height * f64::from(img_h),
    ))
}

/// Convert a normalized oriented-frame crop into a pixel rectangle in the
/// **source** (unoriented) frame.
///
/// Corner pixels are mapped with [`oriented_to_source_pixel`]; the result
/// covers exactly the source pixels visible inside the oriented crop. Returns
/// an error when the crop violates the normalized-frame contract or the
/// source dimensions are zero.
pub fn crop_to_source_rect(
    crop: &CropRect,
    source_width: u32,
    source_height: u32,
    orientation: Orientation,
) -> Result<PixelRect, DevelopError> {
    validate_normalized_crop(crop)?;
    if source_width == 0 || source_height == 0 {
        return Err(DevelopError::Geometry(
            "source dimensions must be non-zero".to_string(),
        ));
    }
    let (oriented_w, oriented_h) = oriented_dimensions(source_width, source_height, orientation);
    let x0 = ((crop.x * f64::from(oriented_w)).round() as u32).min(oriented_w.saturating_sub(1));
    let y0 = ((crop.y * f64::from(oriented_h)).round() as u32).min(oriented_h.saturating_sub(1));
    let x1 = (((crop.x + crop.width) * f64::from(oriented_w)).round() as u32).clamp(1, oriented_w);
    let y1 = (((crop.y + crop.height) * f64::from(oriented_h)).round() as u32).clamp(1, oriented_h);

    let mut min_x = u32::MAX;
    let mut min_y = u32::MAX;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for (cx, cy) in [(x0, y0), (x1 - 1, y0), (x0, y1 - 1), (x1 - 1, y1 - 1)] {
        if let Some((sx, sy)) =
            oriented_to_source_pixel(cx, cy, source_width, source_height, orientation)
        {
            min_x = min_x.min(sx);
            min_y = min_y.min(sy);
            max_x = max_x.max(sx);
            max_y = max_y.max(sy);
        }
    }
    Ok(PixelRect {
        x: min_x,
        y: min_y,
        width: max_x - min_x + 1,
        height: max_y - min_y + 1,
    })
}
