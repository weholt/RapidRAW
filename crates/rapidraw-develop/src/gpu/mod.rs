//! Offscreen GPU processing for the development engine: an ordered WGSL
//! adjustment pipeline (masks, exposure/tone/color/detail/effects, LUT) run
//! against a fully offscreen device context.
//!
//! Separation of concerns (Lap `docs/raw-development/spec.md`, section 2):
//! this module knows nothing about windows, surfaces, Tauri, application
//! state, dialogs, AI runtimes, or network resources. Presentation — the
//! native `WgpuDisplay` — remains in the host adapter. Failures are typed
//! ([`GpuError`]): missing adapters, device loss, out-of-memory, oversized
//! textures and missing resources are explicit results, and an oversized
//! image is never "rendered" as its unprocessed base.

pub mod analytics;
pub mod context;
pub mod error;
pub mod params;
mod processor;
pub mod renderer;

pub use analytics::{
    HistogramBins, OutputColorSpace, apply_gaussian_smoothing, calculate_histogram_from_image,
    histogram_from_rendered, normalize_histogram_range,
};
pub use context::{GpuCapabilities, OffscreenGpuContext};
pub use error::GpuError;
pub use params::{
    AllAdjustments, ColorCalibrationSettings, ColorGradeSettings, GlobalAdjustments, GpuMat3,
    HslColor, MAX_MASKS, MaskAdjustments, MaskDefinition, Point, calculate_agx_matrices_glam,
    get_all_adjustments_from_json, get_global_adjustments_from_json,
    get_mask_adjustments_from_json,
};
pub use renderer::{LutData, OffscreenRenderer, OutputTarget, RenderRequest, RenderedPixels, Roi};
