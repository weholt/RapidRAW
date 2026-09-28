//! High-precision RAW decode and geometry primitives for the RapidRAW/Lap
//! development engine.
//!
//! This crate owns the decode boundary described in Lap
//! `docs/raw-development/spec.md` (module `rapidraw-develop`):
//!
//! - [`decode_original`] turns RAW bytes into a linear, high-precision
//!   ([`f32`]) [`LinearImage`] at the **full decoded dimensions** of the
//!   original. Embedded, downscaled, or 8-bit previews are never produced by
//!   this crate: there is no preview fallback path, and decode failures are
//!   explicit [`DevelopError`]s.
//! - Every effective global interpretation setting — highlight compression,
//!   linear-RAW mode, tone mapper, RAW orientation handling, and
//!   white-balance neutralization — is an explicit [`DecodeOptions`] input.
//!   Nothing is read from app settings stores, environment, or global state.
//! - [`geometry`] applies crop / quarter-turn rotation / flips in a
//!   documented **oriented coordinate system** with explicit conversions
//!   between oriented and source pixel frames.
//! - Cancellation and metadata reporting are separated from the decode math
//!   through a [`CancelToken`] and an optional [`DecodeEvent`] observer.
//!
//! The initial implementation reproduces RapidRAW's `rawler`-based
//! `develop_raw_image` pipeline (revision
//! `5e30bcbb246395d391ba2e9662510641ffe68e6b`, rawler
//! `3289454e9a65f5c973594687cca35602fa8181e3`) so that extraction does not
//! change image appearance. Host-side preprocessing (color noise reduction,
//! sharpening), GPU rendering, tone-mapper *application*, and adjustment math
//! stay in the host for this increment: the tone mapper recorded here is an
//! explicit effective input carried on the report, not applied during decode.
//!
//! The crate must stay free of windows, Tauri `AppState`, dialogs, catalog,
//! filesystem, AI/model, and network dependencies (spec A11). It knows
//! nothing about viewers, workers, or app settings files.

pub mod buffer;
pub mod decode;
pub mod error;
pub mod geometry;
pub mod gpu;
pub mod masks;
pub mod session;
pub mod wb;

pub use buffer::LinearImage;
pub use decode::{
    CancelSource, CancelToken, DecodeEvent, DecodeObserver, DecodeOptions, DecodeReport,
    DecodedOriginal, OrientationHandling, decode_original,
};
pub use error::DevelopError;
pub use geometry::{
    PixelRect, apply_coarse_rotation, apply_crop_normalized, apply_flip, apply_orientation,
    apply_pixel_crop, crop_to_source_rect, oriented_dimensions, oriented_to_source_pixel,
};
pub use masks::{
    BoundedMaskCache, MASK_CACHE_MAX_ENTRIES, MaskRasterError, MaskRasterFrame,
    rasterize_visible_masks, validate_masks_supported, visible_mask_count,
};
pub use rapidraw_edit_model::types::{CropRect, LinearRawMode, ToneMapper};
pub use session::{
    ClosedSession, CommitResult, ExportFrame, ExportJob, ExportJobId, ExportOutcome,
    ExportRenderer, ExportTicket, OpenSessionRequest, OpenedSession, PreviewCacheKey, PreviewFrame,
    PreviewJob, PreviewOutcome, PreviewQuality, PreviewRenderer, PreviewRequest, PreviewTicket,
    PreviewTicketInfo, RecipeStore, SessionDiagnostics, SessionError, SessionId, SessionInfo,
    SessionManager, SessionManagerConfig,
};
pub use wb::{WhiteBalancePolicy, neutralize_wb_if_multiexposure};
