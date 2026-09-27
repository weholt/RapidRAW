//! Typed GPU capability and failure errors for the offscreen processing
//! module. Every failure mode required by Lap `docs/raw-development/spec.md`
//! A7 — missing adapter, device loss, out-of-memory, oversized textures and
//! missing resources — is a distinct variant; the crate never substitutes an
//! unprocessed base image for a failed render.

use std::fmt;

#[derive(Debug, Clone)]
pub enum GpuError {
    /// No compatible wgpu adapter could be found. The message is the
    /// adapter request error text, kept verbatim for visibility.
    NoAdapter(String),
    /// The adapter was found but the logical device could not be created
    /// (limits/features rejected, driver initialization failure).
    DeviceRequest(String),
    /// The device was lost after creation (driver reset, adapter removal).
    /// A device lost after a failed render invalidates the render result.
    DeviceLost(String),
    /// The device ran out of memory allocating a buffer or texture.
    OutOfMemory(String),
    /// The image dimensions exceed the device's 2D texture limit. This is a
    /// truthful unsupported result: callers must not treat the unprocessed
    /// base image as a successful adjusted render.
    TextureTooLarge {
        width: u32,
        height: u32,
        max_dimension: u32,
    },
    /// A resource referenced by the recipe is not available to the render
    /// (for example a LUT flagged in the adjustments but not supplied).
    /// `kind` names the missing resource class ("lut", "mask", ...).
    ResourceMissing { kind: &'static str, detail: String },
    /// The render request itself is invalid (zero dimensions, mismatched
    /// sizes).
    InvalidRequest(String),
    /// A GPU buffer map (readback) failed.
    MapFailed(String),
    /// The device rejected the operation for an unclassified reason.
    Unsupported(String),
    /// An unexpected internal error in the processing module.
    Internal(String),
}

impl GpuError {
    /// Validate image dimensions against the device 2D texture limit.
    /// Returns [`GpuError::TextureTooLarge`] instead of ever accepting an
    /// oversized image.
    pub fn check_texture_dimensions(
        width: u32,
        height: u32,
        max_dimension: u32,
    ) -> Result<(), GpuError> {
        if width == 0 || height == 0 {
            return Err(GpuError::InvalidRequest(format!(
                "invalid texture dimensions {width}x{height}"
            )));
        }
        if width > max_dimension || height > max_dimension {
            return Err(GpuError::TextureTooLarge {
                width,
                height,
                max_dimension,
            });
        }
        Ok(())
    }
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuError::NoAdapter(message) => write!(f, "no usable GPU adapter: {message}"),
            GpuError::DeviceRequest(message) => write!(f, "GPU device request failed: {message}"),
            GpuError::DeviceLost(message) => write!(f, "GPU device lost: {message}"),
            GpuError::OutOfMemory(message) => write!(f, "GPU out of memory: {message}"),
            GpuError::TextureTooLarge {
                width,
                height,
                max_dimension,
            } => write!(
                f,
                "image dimensions {width}x{height} exceed the GPU texture limit \
                 ({max_dimension}); refusing to return the unprocessed image as \
                 a successful adjusted render"
            ),
            GpuError::ResourceMissing { kind, detail } => {
                write!(f, "missing GPU render resource ({kind}): {detail}")
            }
            GpuError::InvalidRequest(message) => write!(f, "invalid GPU render request: {message}"),
            GpuError::MapFailed(message) => write!(f, "GPU readback map failed: {message}"),
            GpuError::Unsupported(message) => write!(f, "unsupported GPU operation: {message}"),
            GpuError::Internal(message) => write!(f, "GPU processing internal error: {message}"),
        }
    }
}

impl std::error::Error for GpuError {}
