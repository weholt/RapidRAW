//! Semantic parameter descriptors: bounds, defaults and slider steps for the
//! flat numeric recipe parameters. Extracted from the RapidRAW adjustment
//! components (`src/components/adjustments/*.tsx`) and modals.
//!
//! The table drives validation of flat scalars and the generated TypeScript
//! range metadata, so it is the single source for those bounds. Nested
//! structures (curves, HSL mixer, color grading, color calibration,
//! parametric curves) are validated structurally in `validate` and documented
//! in the generated interface.

use crate::types::SectionId;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamDescriptor {
    /// Legacy-compatible JSON key of the parameter inside the recipe.
    pub key: &'static str,
    /// Owning bypassable section; `None` for geometry parameters, which are
    /// never bypassed.
    pub section: Option<SectionId>,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub step: f64,
}

const fn p(
    key: &'static str,
    section: Option<SectionId>,
    min: f64,
    max: f64,
    default: f64,
    step: f64,
) -> ParamDescriptor {
    ParamDescriptor {
        key,
        section,
        min,
        max,
        default,
        step,
    }
}

use SectionId::{Basic, Color, Details, Effects};

/// Flat numeric parameters, in recipe declaration order.
pub const PARAM_DESCRIPTORS: &[ParamDescriptor] = &[
    p("exposure", Some(Basic), -5.0, 5.0, 0.0, 0.01),
    p("brightness", Some(Basic), -5.0, 5.0, 0.0, 0.01),
    p("contrast", Some(Basic), -100.0, 100.0, 0.0, 1.0),
    p("highlights", Some(Basic), -100.0, 100.0, 0.0, 1.0),
    p("shadows", Some(Basic), -100.0, 100.0, 0.0, 1.0),
    p("whites", Some(Basic), -100.0, 100.0, 0.0, 1.0),
    p("blacks", Some(Basic), -100.0, 100.0, 0.0, 1.0),
    p("temperature", Some(Color), -100.0, 100.0, 0.0, 1.0),
    p("tint", Some(Color), -100.0, 100.0, 0.0, 1.0),
    p("vibrance", Some(Color), -100.0, 100.0, 0.0, 1.0),
    p("saturation", Some(Color), -100.0, 100.0, 0.0, 1.0),
    p("hue", Some(Color), -180.0, 180.0, 0.0, 1.0),
    p("clarity", Some(Details), -100.0, 100.0, 0.0, 1.0),
    p("structure", Some(Details), -100.0, 100.0, 0.0, 1.0),
    p("dehaze", Some(Details), -100.0, 100.0, 0.0, 1.0),
    p("centré", Some(Details), -100.0, 100.0, 0.0, 1.0),
    p("sharpness", Some(Details), -100.0, 100.0, 0.0, 1.0),
    p("sharpnessThreshold", Some(Details), 0.0, 80.0, 15.0, 1.0),
    p("lumaNoiseReduction", Some(Details), 0.0, 100.0, 0.0, 1.0),
    p("colorNoiseReduction", Some(Details), 0.0, 100.0, 0.0, 1.0),
    p(
        "chromaticAberrationRedCyan",
        Some(Details),
        -100.0,
        100.0,
        0.0,
        1.0,
    ),
    p(
        "chromaticAberrationBlueYellow",
        Some(Details),
        -100.0,
        100.0,
        0.0,
        1.0,
    ),
    p("glowAmount", Some(Effects), 0.0, 100.0, 0.0, 1.0),
    p("halationAmount", Some(Effects), 0.0, 100.0, 0.0, 1.0),
    p("flareAmount", Some(Effects), 0.0, 100.0, 0.0, 1.0),
    p("grainAmount", Some(Effects), 0.0, 100.0, 0.0, 1.0),
    p("grainSize", Some(Effects), 0.0, 100.0, 25.0, 1.0),
    p("grainRoughness", Some(Effects), 0.0, 100.0, 50.0, 1.0),
    p("vignetteAmount", Some(Effects), -100.0, 100.0, 0.0, 1.0),
    p("vignetteMidpoint", Some(Effects), 0.0, 100.0, 50.0, 1.0),
    p("vignetteRoundness", Some(Effects), -100.0, 100.0, 0.0, 1.0),
    p("vignetteFeather", Some(Effects), 0.0, 100.0, 50.0, 1.0),
    p("lutIntensity", Some(Effects), 0.0, 100.0, 100.0, 1.0),
    p("lensBlurAmount", Some(Effects), 0.0, 100.0, 40.0, 1.0),
    p("lensBlurDiffusion", Some(Effects), 0.0, 100.0, 0.0, 1.0),
    p("lensBlurMinDepth", Some(Effects), 0.0, 100.0, 20.0, 1.0),
    p("lensBlurMaxDepth", Some(Effects), 0.0, 100.0, 100.0, 1.0),
    p("lensBlurMinFade", Some(Effects), 0.0, 100.0, 20.0, 1.0),
    p("lensBlurMaxFade", Some(Effects), 0.0, 100.0, 20.0, 1.0),
    p("rotation", None, -180.0, 180.0, 0.0, 0.01),
    p("lensDistortionAmount", None, 0.0, 200.0, 100.0, 1.0),
    p("lensVignetteAmount", None, 0.0, 200.0, 100.0, 1.0),
    p("lensTcaAmount", None, 0.0, 200.0, 100.0, 1.0),
    p("transformDistortion", None, -100.0, 100.0, 0.0, 1.0),
    p("transformVertical", None, -100.0, 100.0, 0.0, 1.0),
    p("transformHorizontal", None, -100.0, 100.0, 0.0, 1.0),
    p("transformRotate", None, -45.0, 45.0, 0.0, 1.0),
    p("transformAspect", None, -100.0, 100.0, 0.0, 1.0),
    p("transformScale", None, 50.0, 150.0, 100.0, 1.0),
    p("transformXOffset", None, -100.0, 100.0, 0.0, 1.0),
    p("transformYOffset", None, -100.0, 100.0, 0.0, 1.0),
];

/// Look up the descriptor default for a legacy-compatible key.
pub fn default_for_key(key: &str) -> Option<f64> {
    PARAM_DESCRIPTORS
        .iter()
        .find(|d| d.key == key)
        .map(|d| d.default)
}
