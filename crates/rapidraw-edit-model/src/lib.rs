//! Validated recipe model for the RapidRAW/Lap development engine.
//!
//! This crate is the single authority for the persisted development recipe:
//! schema types, semantic defaults, bounded finite parameters, validated
//! migrations, canonical serialization and hashing, and the generated
//! TypeScript contract consumed by the RapidRAW frontend (and, at a pinned
//! revision, by Lap).
//!
//! It must stay free of Tauri, React/Vue, filesystem, catalog and AI
//! dependencies. It knows nothing about windows, dialogs, stores, or files.
//!
//! Provenance: parameter semantics are extracted from the RapidRAW source at
//! revision `5e30bcbb246395d391ba2e9662510641ffe68e6b` (`src/utils/adjustments.ts`
//! and `src-tauri/src/app_settings.rs`). Distribution/licensing of the combined
//! product is an unresolved prerequisite (Lap `docs/raw-development/spec.md`,
//! P3); no license field is asserted here.

pub mod canonical;
pub mod contract;
pub mod descriptors;
pub mod geometry;
pub mod masks;
pub mod migrate;
pub mod types;
pub mod validate;

pub use canonical::{canonical_bytes, sha256_hex};
pub use descriptors::{PARAM_DESCRIPTORS, ParamDescriptor};
pub use geometry::{LegacyPixelCrop, crop_to_legacy, crop_to_normalized};
pub use masks::{
    BrushLine, BrushTool, FlowLine, MaskGeometry, MaskPoint, SUPPORTED_KINDS, is_supported_kind,
};
pub use migrate::{
    EnvelopeIdentity, ImportReport, MigrationReport, RECIPE_KEYS, migrate_envelope, parse_envelope,
    recipe_from_legacy_adjustments, saturation_from_lap_legacy, saturation_to_lap_legacy,
};
pub use types::{
    ColorCalibration, ColorGrading, CropRect, CurveMode, CurvePoint, Curves,
    EffectiveDecodeSettings, Hsl, HueSatLum, LensBlurShape, LensCorrectionMode,
    LensDistortionParams, LensProfileRef, LinearRawMode, MaskContainer, MaskLocalAdjustments,
    MaskSectionVisibility, ParametricCurve, ParametricCurveSettings, Recipe, RecipeEnvelope,
    ResourceAlgorithm, ResourceRef, SectionId, SectionVisibility, SubMask, SubMaskMode, ToneMapper,
};
pub use validate::validate_recipe;

/// Current recipe envelope schema version. Migrations reject anything greater.
pub const SCHEMA_VERSION: u32 = 1;

/// Version of this model crate; hosts copy it into `engineVersion`.
pub const MODEL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Errors produced by parsing, validation, migration, and serialization.
#[derive(Debug)]
pub enum ModelError {
    Json(serde_json::Error),
    /// A value violated the schema: path, and the reason.
    Validation(String),
    /// The document declares a schema version this crate cannot read. The
    /// original parsed payload is preserved for diagnostics; callers must
    /// never overwrite such data with defaults.
    UnsupportedSchema {
        found: Option<u32>,
        supported_max: u32,
        preserved: serde_json::Value,
    },
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Json(e) => write!(f, "recipe JSON error: {e}"),
            ModelError::Validation(detail) => write!(f, "recipe validation failed: {detail}"),
            ModelError::UnsupportedSchema {
                found,
                supported_max,
                ..
            } => write!(
                f,
                "recipe schema version {found:?} is newer than the highest supported version {supported_max}; payload preserved, not reset"
            ),
        }
    }
}

impl std::error::Error for ModelError {}

impl From<serde_json::Error> for ModelError {
    fn from(e: serde_json::Error) -> Self {
        ModelError::Json(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_one() {
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
