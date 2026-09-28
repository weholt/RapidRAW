//! Schema types for the versioned recipe envelope.
//!
//! Persisted render data only: UI-only state (clipping overlay, AI patch
//! session state, accordion/zoom/hover state, selected tools) deliberately has
//! no field here. See `docs/raw-development/schema.md` in the Lap checkout.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::descriptors::default_for_key;
use crate::{MODEL_VERSION, SCHEMA_VERSION};

fn descriptor_default(key: &str) -> f64 {
    default_for_key(key).unwrap_or(0.0)
}

macro_rules! set_defaults {
    ($recipe:expr, $($key:literal => $field:ident),+ $(,)?) => {
        $( $recipe.$field = descriptor_default($key); )+
    };
}

/// Bypassable adjustment sections, in canonical processing order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SectionId {
    Basic,
    Curves,
    Color,
    Details,
    Effects,
}

impl SectionId {
    pub const ALL: [SectionId; 5] = [
        SectionId::Basic,
        SectionId::Curves,
        SectionId::Color,
        SectionId::Details,
        SectionId::Effects,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            SectionId::Basic => "basic",
            SectionId::Curves => "curves",
            SectionId::Color => "color",
            SectionId::Details => "details",
            SectionId::Effects => "effects",
        }
    }
}

/// Per-section enable/bypass state (`true` = section applies to the render).
/// This is persisted render state; accordion expansion and similar UI state
/// are not part of the recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SectionVisibility {
    pub basic: bool,
    pub curves: bool,
    pub color: bool,
    pub details: bool,
    pub effects: bool,
}

impl Default for SectionVisibility {
    fn default() -> Self {
        Self {
            basic: true,
            curves: true,
            color: true,
            details: true,
            effects: true,
        }
    }
}

/// Per-mask section state (masks carry their own bypass state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MaskSectionVisibility {
    pub basic: bool,
    pub curves: bool,
    pub color: bool,
    pub details: bool,
    pub effects: bool,
}

impl Default for MaskSectionVisibility {
    fn default() -> Self {
        Self {
            basic: true,
            curves: true,
            color: true,
            details: true,
            effects: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ToneMapper {
    #[serde(rename = "basic")]
    #[default]
    Basic,
    #[serde(rename = "agx")]
    Agx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CurveMode {
    #[serde(rename = "point")]
    #[default]
    Point,
    #[serde(rename = "parametric")]
    Parametric,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LensCorrectionMode {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "manual")]
    #[default]
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LensBlurShape {
    #[serde(rename = "circle")]
    #[default]
    Circle,
    #[serde(rename = "hexagon")]
    Hexagon,
    #[serde(rename = "octagon")]
    Octagon,
    #[serde(rename = "ring")]
    Ring,
}

/// Effective linear-RAW interpretation mode, matching the engine's decode
/// setting strings (`auto`, `gamma`, `skip_calib`, `gamma_skip_calib`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LinearRawMode {
    #[serde(rename = "auto")]
    #[default]
    Auto,
    #[serde(rename = "gamma")]
    Gamma,
    #[serde(rename = "skip_calib")]
    SkipCalib,
    #[serde(rename = "gamma_skip_calib")]
    GammaSkipCalib,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceAlgorithm {
    #[serde(rename = "sha256")]
    Sha256,
}

/// Hash of an external resource (LUT file, depth map, mask bitmap). Recipes
/// reference resources by id in `RecipeEnvelope::resources`; inline resource
/// payloads are never persisted inside a recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRef {
    pub algorithm: ResourceAlgorithm,
    /// Lowercase hex SHA-256 digest (64 characters).
    pub digest: String,
    /// Byte size of the resource when known; bounded.
    pub size_bytes: Option<u64>,
}

/// Effective decode/color interpretation settings captured with the recipe so
/// a render does not depend on the interpreting application's current
/// defaults (highlight compression, linear RAW mode, tonemapper overrides,
/// RAW preprocessing, demosaic speed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EffectiveDecodeSettings {
    pub is_raw: bool,
    pub fast_demosaic: bool,
    /// Engine `raw_highlight_compression`, default 2.5, bounded [1.01, 64].
    pub highlight_compression: f64,
    pub linear_raw_mode: LinearRawMode,
    /// Engine `raw_preprocessing_color_nr`, default 0.5, bounded [0, 1].
    pub raw_color_noise_reduction: f64,
    /// Engine `raw_preprocessing_sharpening`, default 0.35, bounded [0, 1].
    pub raw_sharpening: f64,
    /// Global tonemapper override; `None` means the recipe's own `toneMapper`
    /// applies.
    pub tonemapper_override: Option<ToneMapper>,
}

impl Default for EffectiveDecodeSettings {
    fn default() -> Self {
        Self {
            is_raw: true,
            fast_demosaic: false,
            highlight_compression: 2.5,
            linear_raw_mode: LinearRawMode::Auto,
            raw_color_noise_reduction: 0.5,
            raw_sharpening: 0.35,
            tonemapper_override: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurvePoint {
    pub x: f64,
    pub y: f64,
}

/// Point curves per channel, coordinates in [0, 255] on both axes, endpoints
/// pinned to x = 0 and x = 255 as in the editor UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Curves {
    pub luma: Vec<CurvePoint>,
    pub red: Vec<CurvePoint>,
    pub green: Vec<CurvePoint>,
    pub blue: Vec<CurvePoint>,
}

impl Curves {
    pub fn identity() -> Self {
        Self {
            luma: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 255.0, y: 255.0 },
            ],
            red: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 255.0, y: 255.0 },
            ],
            green: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 255.0, y: 255.0 },
            ],
            blue: vec![
                CurvePoint { x: 0.0, y: 0.0 },
                CurvePoint { x: 255.0, y: 255.0 },
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParametricCurveSettings {
    pub darks: f64,
    pub shadows: f64,
    pub highlights: f64,
    pub lights: f64,
    pub white_level: f64,
    pub black_level: f64,
    pub split1: f64,
    pub split2: f64,
    pub split3: f64,
}

impl Default for ParametricCurveSettings {
    fn default() -> Self {
        Self {
            darks: 0.0,
            shadows: 0.0,
            highlights: 0.0,
            lights: 0.0,
            white_level: 0.0,
            black_level: 0.0,
            split1: 25.0,
            split2: 50.0,
            split3: 75.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ParametricCurve {
    pub luma: ParametricCurveSettings,
    pub red: ParametricCurveSettings,
    pub green: ParametricCurveSettings,
    pub blue: ParametricCurveSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HueSatLum {
    pub hue: f64,
    pub saturation: f64,
    pub luminance: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Hsl {
    pub reds: HueSatLum,
    pub oranges: HueSatLum,
    pub yellows: HueSatLum,
    pub greens: HueSatLum,
    pub aquas: HueSatLum,
    pub blues: HueSatLum,
    pub purples: HueSatLum,
    pub magentas: HueSatLum,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorGrading {
    pub balance: f64,
    pub blending: f64,
    pub global: HueSatLum,
    pub shadows: HueSatLum,
    pub midtones: HueSatLum,
    pub highlights: HueSatLum,
}

impl Default for ColorGrading {
    fn default() -> Self {
        Self {
            balance: 0.0,
            blending: 50.0,
            global: HueSatLum::default(),
            shadows: HueSatLum::default(),
            midtones: HueSatLum::default(),
            highlights: HueSatLum::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ColorCalibration {
    pub shadows_tint: f64,
    pub red_hue: f64,
    pub red_saturation: f64,
    pub green_hue: f64,
    pub green_saturation: f64,
    pub blue_hue: f64,
    pub blue_saturation: f64,
}

/// Crop rectangle in the documented oriented, normalized coordinate system:
/// origin at the top-left of the image after `orientation_steps` quarter
/// turns, unit axes, all components in [0, 1], `x + width <= 1`,
/// `y + height <= 1`. Conversions from legacy pixel crops live in
/// [`crate::geometry`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CropRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Lens distortion coefficients as supplied by the lens-correction backend;
/// keys match the legacy payload (`tca_vr` etc.) so the values stay auditable.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LensDistortionParams {
    pub k1: f64,
    pub k2: f64,
    pub k3: f64,
    pub model: f64,
    #[serde(rename = "tca_vr")]
    pub tca_vr: f64,
    #[serde(rename = "tca_vb")]
    pub tca_vb: f64,
    #[serde(rename = "vig_k1")]
    pub vig_k1: f64,
    #[serde(rename = "vig_k2")]
    pub vig_k2: f64,
    #[serde(rename = "vig_k3")]
    pub vig_k3: f64,
}

/// Provenance of the lens-correction profile that produced the recipe's
/// `lensDistortionParams` (lap-d52): identity (maker/model), an explicit
/// version label, the profile content hash, and the portable resource URI.
/// The envelope's resource map must carry the matching `lens/<sha256>` entry
/// so renders can verify the coefficients were not computed from different
/// profile bytes; a missing entry fails validation, and a missing/changed
/// resource object fails the render explicitly instead of silently changing
/// the export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LensProfileRef {
    /// `resource://lens/<64-hex sha256>`; never an absolute path.
    pub uri: String,
    /// Lens maker as recorded by the profile (display form).
    pub maker: String,
    /// Lens model as recorded by the profile (canonical form).
    pub model: String,
    /// Explicit version label of the profile data (`unversioned` when the
    /// source carries none). Never invented by the engine.
    pub version: String,
    /// Lowercase hex SHA-256 of the profile bytes; equals the URI digest.
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SubMaskMode {
    #[serde(rename = "additive")]
    #[default]
    Additive,
    #[serde(rename = "subtractive")]
    Subtractive,
    #[serde(rename = "intersect")]
    Intersect,
}

/// One sub-mask. The geometry payload (`parameters`) is preserved opaquely
/// and size-bounded so payloads this schema cannot model survive a round
/// trip instead of being dropped. Supported non-AI kinds additionally carry
/// validated typed geometry in `geometry` (see [`crate::masks`]); a
/// supported kind whose legacy payload could not be converted has
/// `geometry: None` and fails explicitly at render time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubMask {
    pub id: String,
    pub name: Option<String>,
    pub invert: bool,
    pub visible: bool,
    pub opacity: f64,
    pub mode: SubMaskMode,
    /// Mask kind (`brush`, `linear`, `radial`, `luminance`, ...), legacy key `type`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Opaque, size-bounded geometry/brush payload (legacy fidelity).
    pub parameters: Value,
    /// Typed, validated geometry for supported kinds; `None` for unsupported
    /// kinds or unconvertible payloads.
    #[serde(default)]
    pub geometry: Option<crate::masks::MaskGeometry>,
}

impl Default for SubMask {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: None,
            invert: false,
            visible: true,
            opacity: 100.0,
            mode: SubMaskMode::Additive,
            kind: String::new(),
            parameters: Value::Null,
            geometry: None,
        }
    }
}

/// Local (per-mask) adjustments: the tone/color/detail/curve subset of the
/// recipe, with mask-wide bounds where the engine differs (noise reduction
/// sliders accept -100..100 inside masks).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MaskLocalAdjustments {
    pub exposure: f64,
    pub brightness: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    pub tone_mapper: ToneMapper,
    pub temperature: f64,
    pub tint: f64,
    pub vibrance: f64,
    pub saturation: f64,
    pub hue: f64,
    pub color_grading: ColorGrading,
    pub hsl: Hsl,
    pub color_calibration: ColorCalibration,
    pub clarity: f64,
    pub structure: f64,
    pub dehaze: f64,
    #[serde(rename = "centré")]
    pub centre: f64,
    pub sharpness: f64,
    pub sharpness_threshold: f64,
    pub luma_noise_reduction: f64,
    pub color_noise_reduction: f64,
    pub chromatic_aberration_red_cyan: f64,
    pub chromatic_aberration_blue_yellow: f64,
    pub glow_amount: f64,
    pub halation_amount: f64,
    pub flare_amount: f64,
    pub curves: Curves,
    pub point_curves: Curves,
    pub parametric_curve: ParametricCurve,
    pub curve_mode: CurveMode,
    pub section_visibility: MaskSectionVisibility,
}

impl Default for MaskLocalAdjustments {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            brightness: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            tone_mapper: ToneMapper::Basic,
            temperature: 0.0,
            tint: 0.0,
            vibrance: 0.0,
            saturation: 0.0,
            hue: 0.0,
            color_grading: ColorGrading::default(),
            hsl: Hsl::default(),
            color_calibration: ColorCalibration::default(),
            clarity: 0.0,
            structure: 0.0,
            dehaze: 0.0,
            centre: 0.0,
            sharpness: 0.0,
            sharpness_threshold: 15.0,
            luma_noise_reduction: 0.0,
            color_noise_reduction: 0.0,
            chromatic_aberration_red_cyan: 0.0,
            chromatic_aberration_blue_yellow: 0.0,
            glow_amount: 0.0,
            halation_amount: 0.0,
            flare_amount: 0.0,
            curves: Curves::identity(),
            point_curves: Curves::identity(),
            parametric_curve: ParametricCurve::default(),
            curve_mode: CurveMode::Point,
            section_visibility: MaskSectionVisibility::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MaskContainer {
    pub id: String,
    pub name: String,
    pub invert: bool,
    pub visible: bool,
    pub opacity: f64,
    pub adjustments: MaskLocalAdjustments,
    pub sub_masks: Vec<SubMask>,
    /// Preserved unsupported fields for this mask (bounded), so unknown
    /// payloads survive instead of being dropped.
    pub unsupported: BTreeMap<String, Value>,
}

impl Default for MaskContainer {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            invert: false,
            visible: true,
            opacity: 100.0,
            adjustments: MaskLocalAdjustments::default(),
            sub_masks: Vec::new(),
            unsupported: BTreeMap::new(),
        }
    }
}

/// The semantic recipe: every persisted render parameter extracted from the
/// legacy `INITIAL_ADJUSTMENTS` shape. UI-only legacy fields (`showClipping`,
/// `aiPatches`) are excluded; unknown legacy fields are preserved at the
/// envelope level during import, never silently dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Recipe {
    // Basic / tone
    pub exposure: f64,
    pub brightness: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    pub tone_mapper: ToneMapper,
    // Curves
    pub curves: Curves,
    pub point_curves: Curves,
    pub parametric_curve: ParametricCurve,
    pub curve_mode: CurveMode,
    // Color
    pub temperature: f64,
    pub tint: f64,
    pub vibrance: f64,
    pub saturation: f64,
    pub hue: f64,
    pub color_grading: ColorGrading,
    pub hsl: Hsl,
    pub color_calibration: ColorCalibration,
    // Details
    pub clarity: f64,
    pub structure: f64,
    pub dehaze: f64,
    #[serde(rename = "centré")]
    pub centre: f64,
    pub sharpness: f64,
    pub sharpness_threshold: f64,
    pub luma_noise_reduction: f64,
    pub color_noise_reduction: f64,
    pub chromatic_aberration_red_cyan: f64,
    pub chromatic_aberration_blue_yellow: f64,
    // Effects
    pub glow_amount: f64,
    pub halation_amount: f64,
    pub flare_amount: f64,
    pub grain_amount: f64,
    pub grain_size: f64,
    pub grain_roughness: f64,
    pub vignette_amount: f64,
    pub vignette_midpoint: f64,
    pub vignette_roundness: f64,
    pub vignette_feather: f64,
    // LUT (data itself is an external resource, never inline)
    pub lut_intensity: f64,
    pub lut_is_scene_referred: bool,
    pub lut_name: Option<String>,
    pub lut_path: Option<String>,
    pub lut_size: u32,
    // Lens blur
    pub lens_blur_enabled: bool,
    pub lens_blur_amount: f64,
    pub lens_blur_diffusion: f64,
    pub lens_blur_shape: LensBlurShape,
    pub lens_blur_depth_map: Option<String>,
    pub lens_blur_min_depth: f64,
    pub lens_blur_max_depth: f64,
    pub lens_blur_min_fade: f64,
    pub lens_blur_max_fade: f64,
    // Geometry (oriented coordinate system, see CropRect)
    pub rotation: f64,
    pub orientation_steps: u32,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub crop: Option<CropRect>,
    pub aspect_ratio: Option<f64>,
    // Perspective/geometry transform
    pub transform_distortion: f64,
    pub transform_vertical: f64,
    pub transform_horizontal: f64,
    pub transform_rotate: f64,
    pub transform_aspect: f64,
    pub transform_scale: f64,
    pub transform_x_offset: f64,
    pub transform_y_offset: f64,
    // Lens correction
    pub lens_correction_mode: LensCorrectionMode,
    pub lens_maker: Option<String>,
    pub lens_model: Option<String>,
    pub lens_distortion_amount: f64,
    pub lens_vignette_amount: f64,
    pub lens_tca_amount: f64,
    pub lens_distortion_enabled: bool,
    pub lens_tca_enabled: bool,
    pub lens_vignette_enabled: bool,
    pub lens_distortion_params: Option<LensDistortionParams>,
    /// Provenance of the profile the coefficients were resolved from.
    pub lens_profile: Option<LensProfileRef>,
    // Masks
    pub masks: Vec<MaskContainer>,
    // Section bypass state and canonical operation order
    pub section_visibility: SectionVisibility,
    pub section_order: Vec<SectionId>,
}

impl Default for Recipe {
    fn default() -> Self {
        let mut recipe = Recipe {
            exposure: 0.0,
            brightness: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            tone_mapper: ToneMapper::Basic,
            curves: Curves::identity(),
            point_curves: Curves::identity(),
            parametric_curve: ParametricCurve::default(),
            curve_mode: CurveMode::Point,
            temperature: 0.0,
            tint: 0.0,
            vibrance: 0.0,
            // RapidRAW semantics: 0 is the neutral saturation value.
            saturation: 0.0,
            hue: 0.0,
            color_grading: ColorGrading::default(),
            hsl: Hsl::default(),
            color_calibration: ColorCalibration::default(),
            clarity: 0.0,
            structure: 0.0,
            dehaze: 0.0,
            centre: 0.0,
            sharpness: 0.0,
            sharpness_threshold: 15.0,
            luma_noise_reduction: 0.0,
            color_noise_reduction: 0.0,
            chromatic_aberration_red_cyan: 0.0,
            chromatic_aberration_blue_yellow: 0.0,
            glow_amount: 0.0,
            halation_amount: 0.0,
            flare_amount: 0.0,
            grain_amount: 0.0,
            grain_size: 25.0,
            grain_roughness: 50.0,
            vignette_amount: 0.0,
            vignette_midpoint: 50.0,
            vignette_roundness: 0.0,
            vignette_feather: 50.0,
            lut_intensity: 100.0,
            lut_is_scene_referred: false,
            lut_name: None,
            lut_path: None,
            lut_size: 0,
            lens_blur_enabled: false,
            lens_blur_amount: 40.0,
            lens_blur_diffusion: 0.0,
            lens_blur_shape: LensBlurShape::Circle,
            lens_blur_depth_map: None,
            lens_blur_min_depth: 20.0,
            lens_blur_max_depth: 100.0,
            lens_blur_min_fade: 20.0,
            lens_blur_max_fade: 20.0,
            rotation: 0.0,
            orientation_steps: 0,
            flip_horizontal: false,
            flip_vertical: false,
            crop: None,
            aspect_ratio: None,
            transform_distortion: 0.0,
            transform_vertical: 0.0,
            transform_horizontal: 0.0,
            transform_rotate: 0.0,
            transform_aspect: 0.0,
            transform_scale: 100.0,
            transform_x_offset: 0.0,
            transform_y_offset: 0.0,
            lens_correction_mode: LensCorrectionMode::Manual,
            lens_maker: None,
            lens_model: None,
            lens_distortion_amount: 100.0,
            lens_vignette_amount: 100.0,
            lens_tca_amount: 100.0,
            lens_distortion_enabled: true,
            lens_tca_enabled: true,
            lens_vignette_enabled: true,
            lens_distortion_params: None,
            lens_profile: None,
            masks: Vec::new(),
            section_visibility: SectionVisibility::default(),
            section_order: SectionId::ALL.to_vec(),
        };
        // Cross-check the explicitly set scalars against the descriptor table
        // so a table edit that forgets the struct default (or vice versa) is
        // caught in tests, not in production recipes.
        set_defaults!(
            recipe,
            "exposure" => exposure,
            "brightness" => brightness,
            "contrast" => contrast,
            "highlights" => highlights,
            "shadows" => shadows,
            "whites" => whites,
            "blacks" => blacks,
            "temperature" => temperature,
            "tint" => tint,
            "vibrance" => vibrance,
            "saturation" => saturation,
            "hue" => hue,
            "clarity" => clarity,
            "structure" => structure,
            "dehaze" => dehaze,
            "centré" => centre,
            "sharpness" => sharpness,
            "sharpnessThreshold" => sharpness_threshold,
            "lumaNoiseReduction" => luma_noise_reduction,
            "colorNoiseReduction" => color_noise_reduction,
            "chromaticAberrationRedCyan" => chromatic_aberration_red_cyan,
            "chromaticAberrationBlueYellow" => chromatic_aberration_blue_yellow,
            "glowAmount" => glow_amount,
            "halationAmount" => halation_amount,
            "flareAmount" => flare_amount,
            "grainAmount" => grain_amount,
            "grainSize" => grain_size,
            "grainRoughness" => grain_roughness,
            "vignetteAmount" => vignette_amount,
            "vignetteMidpoint" => vignette_midpoint,
            "vignetteRoundness" => vignette_roundness,
            "vignetteFeather" => vignette_feather,
            "lutIntensity" => lut_intensity,
            "lensBlurAmount" => lens_blur_amount,
            "lensBlurDiffusion" => lens_blur_diffusion,
            "lensBlurMinDepth" => lens_blur_min_depth,
            "lensBlurMaxDepth" => lens_blur_max_depth,
            "lensBlurMinFade" => lens_blur_min_fade,
            "lensBlurMaxFade" => lens_blur_max_fade,
            "rotation" => rotation,
            "lensDistortionAmount" => lens_distortion_amount,
            "lensVignetteAmount" => lens_vignette_amount,
            "lensTcaAmount" => lens_tca_amount,
            "transformDistortion" => transform_distortion,
            "transformVertical" => transform_vertical,
            "transformHorizontal" => transform_horizontal,
            "transformRotate" => transform_rotate,
            "transformAspect" => transform_aspect,
            "transformScale" => transform_scale,
            "transformXOffset" => transform_x_offset,
            "transformYOffset" => transform_y_offset
        );
        recipe
    }
}

/// The durable envelope: identity, schema version, effective decode/color
/// settings, the ordered recipe, bypass state, resource hashes, and preserved
/// unsupported payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RecipeEnvelope {
    pub schema_version: u32,
    /// Version of the model/engine writing the envelope.
    pub engine_version: String,
    pub asset_id: String,
    pub variant_id: String,
    /// Monotonic per asset/variant; used for optimistic concurrency.
    pub revision: u64,
    /// SHA-256 (lowercase hex) of the untouched source image bytes.
    pub source_fingerprint: String,
    pub decode: EffectiveDecodeSettings,
    pub recipe: Recipe,
    /// Resource content hashes, keyed by stable resource id.
    pub resources: BTreeMap<String, ResourceRef>,
    /// Preserved payloads this schema version does not model (legacy extras,
    /// future fields). Bounded; never silently discarded.
    pub unsupported: BTreeMap<String, Value>,
}

impl RecipeEnvelope {
    pub fn new(
        engine_version: &str,
        asset_id: &str,
        variant_id: &str,
        source_fingerprint: &str,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            engine_version: engine_version.to_string(),
            asset_id: asset_id.to_string(),
            variant_id: variant_id.to_string(),
            revision: 1,
            source_fingerprint: source_fingerprint.to_string(),
            decode: EffectiveDecodeSettings::default(),
            recipe: Recipe::default(),
            resources: BTreeMap::new(),
            unsupported: BTreeMap::new(),
        }
    }
}

impl Default for RecipeEnvelope {
    fn default() -> Self {
        Self::new(MODEL_VERSION, "asset", "primary", &"0".repeat(64))
    }
}
