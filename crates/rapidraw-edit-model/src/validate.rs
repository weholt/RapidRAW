//! Validation: finite numbers, bounds, structural limits, identities,
//! resources, and payload size bounds. Bounds for flat scalars come from the
//! descriptor table; nested structures are validated structurally here.

use crate::ModelError;
use crate::descriptors::PARAM_DESCRIPTORS;
use crate::geometry::validate_crop;
use crate::types::{
    ColorCalibration, ColorGrading, CurvePoint, Curves, EffectiveDecodeSettings, Hsl,
    LensDistortionParams, MaskContainer, ParametricCurve, Recipe, RecipeEnvelope,
    ResourceAlgorithm, ResourceRef, SectionId, SubMask,
};
use serde_json::Value;

const MAX_CURVE_POINTS: usize = 32;
const MAX_CURVE_COORD: f64 = 255.0;
const MAX_MASKS: usize = 32;
const MAX_SUB_MASKS: usize = 16;
const MAX_MASK_UNSUPPORTED_ENTRIES: usize = 16;
const MAX_MASK_UNSUPPORTED_BYTES: usize = 32 * 1024;
const MAX_SUB_MASK_PARAMETERS_BYTES: usize = 64 * 1024;
const MAX_ENVELOPE_UNSUPPORTED_ENTRIES: usize = 64;
const MAX_ENVELOPE_UNSUPPORTED_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESOURCE_ENTRIES: usize = 256;
const MAX_RESOURCE_BYTES: u64 = 1_073_741_824;
const MAX_LUT_SIZE: u32 = 4096;

pub fn validate_recipe(recipe: &Recipe) -> Result<(), ModelError> {
    let value = serde_json::to_value(recipe)?;
    let obj = value
        .as_object()
        .ok_or_else(|| ModelError::Validation("recipe is not an object".to_string()))?;
    for d in PARAM_DESCRIPTORS {
        let v = obj.get(d.key).and_then(Value::as_f64).ok_or_else(|| {
            ModelError::Validation(format!("recipe.{} is missing or not a number", d.key))
        })?;
        if !v.is_finite() {
            return Err(ModelError::Validation(format!(
                "recipe.{} must be finite, got {v}",
                d.key
            )));
        }
        if v < d.min || v > d.max {
            return Err(ModelError::Validation(format!(
                "recipe.{} value {v} outside bounds [{}, {}]",
                d.key, d.min, d.max
            )));
        }
    }

    validate_levels(&recipe.levels)?;
    validate_curves(&recipe.curves, "recipe.curves")?;
    validate_curves(&recipe.point_curves, "recipe.pointCurves")?;
    validate_parametric(&recipe.parametric_curve, "recipe.parametricCurve")?;
    validate_color_grading(&recipe.color_grading, "recipe.colorGrading")?;
    validate_hsl(&recipe.hsl, "recipe.hsl")?;
    validate_color_calibration(&recipe.color_calibration, "recipe.colorCalibration")?;

    check_opt_string(recipe.lut_name.as_deref(), "recipe.lutName", 200)?;
    check_opt_string(recipe.lut_path.as_deref(), "recipe.lutPath", 1024)?;
    if recipe.lut_size > MAX_LUT_SIZE {
        return Err(ModelError::Validation(format!(
            "recipe.lutSize {} exceeds {MAX_LUT_SIZE}",
            recipe.lut_size
        )));
    }
    check_opt_string(
        recipe.lens_blur_depth_map.as_deref(),
        "recipe.lensBlurDepthMap",
        512,
    )?;
    check_opt_string(recipe.lens_maker.as_deref(), "recipe.lensMaker", 120)?;
    check_opt_string(recipe.lens_model.as_deref(), "recipe.lensModel", 120)?;
    if let Some(profile) = &recipe.lens_profile {
        validate_lens_profile(profile)?;
    }
    if recipe.orientation_steps > 3 {
        return Err(ModelError::Validation(format!(
            "recipe.orientationSteps {} outside 0..=3",
            recipe.orientation_steps
        )));
    }
    if let Some(crop) = &recipe.crop {
        validate_crop(crop)?;
    }
    if let Some(ar) = recipe.aspect_ratio
        && !(0.01..=100.0).contains(&ar)
    {
        return Err(ModelError::Validation(format!(
            "recipe.aspectRatio {ar} outside [0.01, 100]"
        )));
    }
    if let Some(params) = &recipe.lens_distortion_params {
        validate_lens_params(params)?;
    }

    if recipe.masks.len() > MAX_MASKS {
        return Err(ModelError::Validation(format!(
            "recipe.masks count {} exceeds {MAX_MASKS}",
            recipe.masks.len()
        )));
    }
    for (index, mask) in recipe.masks.iter().enumerate() {
        validate_mask(mask, index)?;
    }

    validate_section_order(&recipe.section_order)?;
    Ok(())
}

impl Recipe {
    pub fn validate(&self) -> Result<(), ModelError> {
        validate_recipe(self)
    }
}

fn validate_section_order(order: &[SectionId]) -> Result<(), ModelError> {
    let mut seen = [false; 5];
    if order.len() != 5 {
        return Err(ModelError::Validation(format!(
            "recipe.sectionOrder must contain exactly the 5 canonical sections, got {} entries",
            order.len()
        )));
    }
    for section in order {
        let index = match section {
            SectionId::Basic => 0,
            SectionId::Curves => 1,
            SectionId::Color => 2,
            SectionId::Details => 3,
            SectionId::Effects => 4,
        };
        if seen[index] {
            return Err(ModelError::Validation(
                "recipe.sectionOrder contains a duplicate section".to_string(),
            ));
        }
        seen[index] = true;
    }
    Ok(())
}

fn check_string(value: &str, path: &str, max_len: usize) -> Result<(), ModelError> {
    if value.chars().count() > max_len {
        return Err(ModelError::Validation(format!(
            "{path} exceeds {max_len} characters"
        )));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(ModelError::Validation(format!(
            "{path} contains control characters"
        )));
    }
    Ok(())
}

fn check_opt_string(value: Option<&str>, path: &str, max_len: usize) -> Result<(), ModelError> {
    match value {
        Some(v) => check_string(v, path, max_len),
        None => Ok(()),
    }
}

fn check_num(value: f64, path: &str, min: f64, max: f64) -> Result<(), ModelError> {
    if !value.is_finite() {
        return Err(ModelError::Validation(format!(
            "{path} must be finite, got {value}"
        )));
    }
    if value < min || value > max {
        return Err(ModelError::Validation(format!(
            "{path} value {value} outside bounds [{min}, {max}]"
        )));
    }
    Ok(())
}

fn validate_curves(curves: &Curves, path: &str) -> Result<(), ModelError> {
    for (channel, points) in [
        ("luma", &curves.luma),
        ("red", &curves.red),
        ("green", &curves.green),
        ("blue", &curves.blue),
    ] {
        validate_curve_points(points, &format!("{path}.{channel}"))?;
    }
    Ok(())
}

fn validate_curve_points(points: &[CurvePoint], path: &str) -> Result<(), ModelError> {
    if points.len() < 2 || points.len() > MAX_CURVE_POINTS {
        return Err(ModelError::Validation(format!(
            "{path} must have between 2 and {MAX_CURVE_POINTS} points, got {}",
            points.len()
        )));
    }
    let mut last_x = -1.0;
    for (i, point) in points.iter().enumerate() {
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(ModelError::Validation(format!(
                "{path}.point{i} coordinates must be finite"
            )));
        }
        if !(0.0..=MAX_CURVE_COORD).contains(&point.x)
            || !(0.0..=MAX_CURVE_COORD).contains(&point.y)
        {
            return Err(ModelError::Validation(format!(
                "{path}.point{i} coordinates outside [0, 255]"
            )));
        }
        if i == 0 && point.x != 0.0 {
            return Err(ModelError::Validation(format!("{path} must start at x=0")));
        }
        if i == points.len() - 1 && point.x != MAX_CURVE_COORD {
            return Err(ModelError::Validation(format!("{path} must end at x=255")));
        }
        if point.x <= last_x {
            return Err(ModelError::Validation(format!(
                "{path} point x values must be strictly increasing"
            )));
        }
        last_x = point.x;
    }
    Ok(())
}

fn validate_parametric(curve: &ParametricCurve, path: &str) -> Result<(), ModelError> {
    for channel in ["luma", "red", "green", "blue"] {
        let settings = match channel {
            "luma" => &curve.luma,
            "red" => &curve.red,
            "green" => &curve.green,
            "blue" => &curve.blue,
            _ => unreachable!(),
        };
        for (field, value) in [
            ("darks", settings.darks),
            ("shadows", settings.shadows),
            ("highlights", settings.highlights),
            ("lights", settings.lights),
            ("whiteLevel", settings.white_level),
            ("blackLevel", settings.black_level),
        ] {
            check_num(value, &format!("{path}.{channel}.{field}"), -100.0, 100.0)?;
        }
        for (field, value) in [
            ("split1", settings.split1),
            ("split2", settings.split2),
            ("split3", settings.split3),
        ] {
            check_num(value, &format!("{path}.{channel}.{field}"), 0.0, 100.0)?;
        }
    }
    Ok(())
}

fn validate_color_grading(grading: &ColorGrading, path: &str) -> Result<(), ModelError> {
    check_num(grading.balance, &format!("{path}.balance"), -100.0, 100.0)?;
    check_num(grading.blending, &format!("{path}.blending"), 0.0, 100.0)?;
    for zone in ["global", "shadows", "midtones", "highlights"] {
        let value = match zone {
            "global" => &grading.global,
            "shadows" => &grading.shadows,
            "midtones" => &grading.midtones,
            "highlights" => &grading.highlights,
            _ => unreachable!(),
        };
        // Grading chooses an absolute hue in degrees; HSL channel edits are
        // signed offsets instead. Retain the previously accepted negative
        // range so existing recipes keep their exact rendered appearance.
        check_num(value.hue, &format!("{path}.{zone}.hue"), -100.0, 360.0)?;
        check_num(
            value.saturation,
            &format!("{path}.{zone}.saturation"),
            -100.0,
            100.0,
        )?;
        check_num(
            value.luminance,
            &format!("{path}.{zone}.luminance"),
            -100.0,
            100.0,
        )?;
    }
    Ok(())
}

fn validate_hsl(hsl: &Hsl, path: &str) -> Result<(), ModelError> {
    for channel in [
        "reds", "oranges", "yellows", "greens", "aquas", "blues", "purples", "magentas",
    ] {
        let value = match channel {
            "reds" => &hsl.reds,
            "oranges" => &hsl.oranges,
            "yellows" => &hsl.yellows,
            "greens" => &hsl.greens,
            "aquas" => &hsl.aquas,
            "blues" => &hsl.blues,
            "purples" => &hsl.purples,
            "magentas" => &hsl.magentas,
            _ => unreachable!(),
        };
        validate_hsl_component(value, &format!("{path}.{channel}"))?;
    }
    Ok(())
}

fn validate_hsl_component(
    component: &crate::types::HueSatLum,
    path: &str,
) -> Result<(), ModelError> {
    check_num(component.hue, &format!("{path}.hue"), -100.0, 100.0)?;
    check_num(
        component.saturation,
        &format!("{path}.saturation"),
        -100.0,
        100.0,
    )?;
    check_num(
        component.luminance,
        &format!("{path}.luminance"),
        -100.0,
        100.0,
    )?;
    Ok(())
}

fn validate_color_calibration(
    calibration: &ColorCalibration,
    path: &str,
) -> Result<(), ModelError> {
    for (field, value) in [
        ("shadowsTint", calibration.shadows_tint),
        ("redHue", calibration.red_hue),
        ("redSaturation", calibration.red_saturation),
        ("greenHue", calibration.green_hue),
        ("greenSaturation", calibration.green_saturation),
        ("blueHue", calibration.blue_hue),
        ("blueSaturation", calibration.blue_saturation),
    ] {
        check_num(value, &format!("{path}.{field}"), -100.0, 100.0)?;
    }
    Ok(())
}

/// Lowercase-hex SHA-256 digest check shared by lens-profile validation.
fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate_lens_profile(profile: &crate::types::LensProfileRef) -> Result<(), ModelError> {
    let path = "recipe.lensProfile";
    check_string(&profile.uri, path, 128)?;
    let Some(digest) = profile.uri.strip_prefix("resource://lens/") else {
        return Err(ModelError::Validation(format!(
            "{path}.uri must be a resource://lens/<sha256> reference, got '{}'",
            profile.uri
        )));
    };
    if !is_lower_hex_digest(digest) {
        return Err(ModelError::Validation(format!(
            "{path}.uri digest must be 64 lowercase hex characters"
        )));
    }
    if !is_lower_hex_digest(&profile.sha256) {
        return Err(ModelError::Validation(format!(
            "{path}.sha256 must be 64 lowercase hex characters"
        )));
    }
    if digest != profile.sha256 {
        return Err(ModelError::Validation(format!(
            "{path}.uri digest {digest} does not match sha256 {}",
            profile.sha256
        )));
    }
    if profile.maker.is_empty() {
        return Err(ModelError::Validation(format!(
            "{path}.maker must not be empty"
        )));
    }
    check_string(&profile.maker, &format!("{path}.maker"), 120)?;
    if profile.model.is_empty() {
        return Err(ModelError::Validation(format!(
            "{path}.model must not be empty"
        )));
    }
    check_string(&profile.model, &format!("{path}.model"), 120)?;
    if profile.version.is_empty() {
        return Err(ModelError::Validation(format!(
            "{path}.version must not be empty (use 'unversioned' when the source carries no version)"
        )));
    }
    check_string(&profile.version, &format!("{path}.version"), 120)?;
    Ok(())
}

fn validate_lens_params(params: &LensDistortionParams) -> Result<(), ModelError> {
    for (field, value) in [
        ("k1", params.k1),
        ("k2", params.k2),
        ("k3", params.k3),
        ("vig_k1", params.vig_k1),
        ("vig_k2", params.vig_k2),
        ("vig_k3", params.vig_k3),
    ] {
        check_num(
            value,
            &format!("recipe.lensDistortionParams.{field}"),
            -10.0,
            10.0,
        )?;
    }
    check_num(
        params.model,
        "recipe.lensDistortionParams.model",
        0.0,
        100.0,
    )?;
    check_num(
        params.tca_vr,
        "recipe.lensDistortionParams.tca_vr",
        0.5,
        2.0,
    )?;
    check_num(
        params.tca_vb,
        "recipe.lensDistortionParams.tca_vb",
        0.5,
        2.0,
    )?;
    Ok(())
}

/// Flat numeric fields shared with mask-local adjustments. Mask noise
/// reduction sliders legitimately reach -100 inside masks, so their floor is
/// widened here relative to the global descriptor bounds.
const MASK_LOCAL_NUMERIC_KEYS: &[&str] = &[
    "exposure",
    "brightness",
    "contrast",
    "highlights",
    "shadows",
    "whites",
    "blacks",
    "temperature",
    "tint",
    "vibrance",
    "saturation",
    "hue",
    "clarity",
    "structure",
    "dehaze",
    "centré",
    "sharpness",
    "sharpnessThreshold",
    "lumaNoiseReduction",
    "colorNoiseReduction",
    "chromaticAberrationRedCyan",
    "chromaticAberrationBlueYellow",
    "glowAmount",
    "halationAmount",
    "flareAmount",
];

fn validate_mask(mask: &MaskContainer, index: usize) -> Result<(), ModelError> {
    let path = format!("recipe.masks[{index}]");
    if mask.id.is_empty() {
        return Err(ModelError::Validation(format!(
            "{path}.id must not be empty"
        )));
    }
    check_string(&mask.id, &format!("{path}.id"), 64)?;
    check_string(&mask.name, &format!("{path}.name"), 200)?;
    check_num(mask.opacity, &format!("{path}.opacity"), 0.0, 100.0)?;

    if mask.sub_masks.len() > MAX_SUB_MASKS {
        return Err(ModelError::Validation(format!(
            "{path}.subMasks count {} exceeds {MAX_SUB_MASKS}",
            mask.sub_masks.len()
        )));
    }
    for (sub_index, sub_mask) in mask.sub_masks.iter().enumerate() {
        validate_sub_mask(sub_mask, &format!("{path}.subMasks[{sub_index}]"))?;
    }

    let adjustments = serde_json::to_value(&mask.adjustments)?;
    let adj_obj = adjustments
        .as_object()
        .ok_or_else(|| ModelError::Validation(format!("{path}.adjustments is not an object")))?;
    for key in MASK_LOCAL_NUMERIC_KEYS {
        let value = adj_obj.get(*key).and_then(Value::as_f64).ok_or_else(|| {
            ModelError::Validation(format!("{path}.adjustments.{key} missing or not a number"))
        })?;
        let (min, max) = mask_bounds_for(key);
        check_num(value, &format!("{path}.adjustments.{key}"), min, max)?;
    }
    validate_curves(
        &mask.adjustments.curves,
        &format!("{path}.adjustments.curves"),
    )?;
    validate_curves(
        &mask.adjustments.point_curves,
        &format!("{path}.adjustments.pointCurves"),
    )?;
    validate_parametric(
        &mask.adjustments.parametric_curve,
        &format!("{path}.adjustments.parametricCurve"),
    )?;
    validate_color_grading(
        &mask.adjustments.color_grading,
        &format!("{path}.adjustments.colorGrading"),
    )?;
    validate_hsl(&mask.adjustments.hsl, &format!("{path}.adjustments.hsl"))?;
    validate_color_calibration(
        &mask.adjustments.color_calibration,
        &format!("{path}.adjustments.colorCalibration"),
    )?;

    if mask.unsupported.len() > MAX_MASK_UNSUPPORTED_ENTRIES {
        return Err(ModelError::Validation(format!(
            "{path}.unsupported has too many entries ({})",
            mask.unsupported.len()
        )));
    }
    let total: usize = mask
        .unsupported
        .values()
        .map(canonical_len)
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .sum();
    if total > MAX_MASK_UNSUPPORTED_BYTES {
        return Err(ModelError::Validation(format!(
            "{path}.unsupported payload of {total} bytes exceeds {MAX_MASK_UNSUPPORTED_BYTES}"
        )));
    }
    Ok(())
}

fn mask_bounds_for(key: &str) -> (f64, f64) {
    let descriptor = PARAM_DESCRIPTORS
        .iter()
        .find(|d| d.key == key)
        .expect("mask numeric keys must exist in the descriptor table");
    if key == "lumaNoiseReduction" || key == "colorNoiseReduction" {
        (-100.0, descriptor.max)
    } else {
        (descriptor.min, descriptor.max)
    }
}

fn validate_sub_mask(sub_mask: &SubMask, path: &str) -> Result<(), ModelError> {
    if sub_mask.id.is_empty() {
        return Err(ModelError::Validation(format!(
            "{path}.id must not be empty"
        )));
    }
    check_string(&sub_mask.id, &format!("{path}.id"), 64)?;
    check_opt_string(sub_mask.name.as_deref(), &format!("{path}.name"), 200)?;
    check_num(sub_mask.opacity, &format!("{path}.opacity"), 0.0, 100.0)?;
    if sub_mask.kind.is_empty() {
        return Err(ModelError::Validation(format!(
            "{path}.type must not be empty"
        )));
    }
    check_string(&sub_mask.kind, &format!("{path}.type"), 32)?;
    if canonical_len(&sub_mask.parameters)? > MAX_SUB_MASK_PARAMETERS_BYTES {
        return Err(ModelError::Validation(format!(
            "{path}.parameters payload exceeds {MAX_SUB_MASK_PARAMETERS_BYTES} bytes"
        )));
    }
    if let Some(geometry) = &sub_mask.geometry {
        geometry.validate().map_err(|err| match err {
            ModelError::Validation(detail) => {
                ModelError::Validation(format!("{path}.geometry: {detail}"))
            }
            other => other,
        })?;
    }
    Ok(())
}

fn canonical_len(value: &Value) -> Result<usize, ModelError> {
    Ok(crate::canonical::canonical_bytes(value)?.len())
}

impl EffectiveDecodeSettings {
    fn validate(&self) -> Result<(), ModelError> {
        check_num(
            self.highlight_compression,
            "decode.highlightCompression",
            1.01,
            64.0,
        )?;
        check_num(
            self.raw_color_noise_reduction,
            "decode.rawColorNoiseReduction",
            0.0,
            1.0,
        )?;
        check_num(self.raw_sharpening, "decode.rawSharpening", 0.0, 1.0)?;
        Ok(())
    }
}

impl ResourceRef {
    fn validate(&self, path: &str) -> Result<(), ModelError> {
        if self.digest.len() != 64 || !self.digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ModelError::Validation(format!(
                "{path}.digest must be a 64-character hex string"
            )));
        }
        if self.digest.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(ModelError::Validation(format!(
                "{path}.digest must be lowercase hex"
            )));
        }
        if let Some(size) = self.size_bytes
            && size > MAX_RESOURCE_BYTES
        {
            return Err(ModelError::Validation(format!(
                "{path}.sizeBytes {size} exceeds {MAX_RESOURCE_BYTES}"
            )));
        }
        let _ = self.algorithm == ResourceAlgorithm::Sha256;
        Ok(())
    }
}

impl RecipeEnvelope {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.schema_version != crate::SCHEMA_VERSION {
            return Err(ModelError::UnsupportedSchema {
                found: Some(self.schema_version),
                supported_max: crate::SCHEMA_VERSION,
                preserved: serde_json::to_value(self)?,
            });
        }
        check_string(&self.engine_version, "engineVersion", 64)?;
        if self.engine_version.is_empty() {
            return Err(ModelError::Validation(
                "engineVersion must not be empty".to_string(),
            ));
        }
        for (path, value) in [("assetId", &self.asset_id), ("variantId", &self.variant_id)] {
            if value.is_empty() {
                return Err(ModelError::Validation(format!("{path} must not be empty")));
            }
            check_string(value, path, 256)?;
        }
        if self.revision == 0 {
            return Err(ModelError::Validation("revision must be >= 1".to_string()));
        }
        if self.source_fingerprint.len() != 64
            || !self
                .source_fingerprint
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ModelError::Validation(
                "sourceFingerprint must be 64 lowercase hex characters".to_string(),
            ));
        }
        self.decode.validate()?;
        self.recipe.validate()?;

        // A recipe's lens-profile provenance must have its content-addressed
        // resource entry, and the entry's digest must agree with the recipe's
        // recorded hash — otherwise renders could never verify the
        // coefficients against the profile bytes they claim to come from.
        if let Some(profile) = &self.recipe.lens_profile {
            let expected_key = format!("lens/{}", profile.sha256);
            let entry = self.resources.get(&expected_key).ok_or_else(|| {
                ModelError::Validation(format!(
                    "recipe.lensProfile references {} but the resource map has no '{expected_key}' entry",
                    profile.uri
                ))
            })?;
            if entry.digest != profile.sha256 {
                return Err(ModelError::Validation(format!(
                    "recipe.lensProfile sha256 {} disagrees with the resources['{expected_key}'] digest {}",
                    profile.sha256, entry.digest
                )));
            }
        }

        if self.resources.len() > MAX_RESOURCE_ENTRIES {
            return Err(ModelError::Validation(
                "resources map exceeds 256 entries".to_string(),
            ));
        }
        for (key, resource) in &self.resources {
            if key.is_empty() || key.chars().count() > 256 {
                return Err(ModelError::Validation(
                    "resource keys must be 1..=256 characters".to_string(),
                ));
            }
            resource.validate(&format!("resources[{key}]"))?;
        }

        if self.unsupported.len() > MAX_ENVELOPE_UNSUPPORTED_ENTRIES {
            return Err(ModelError::Validation(
                "unsupported payload has too many entries".to_string(),
            ));
        }
        let mut total = 0usize;
        for value in self.unsupported.values() {
            total += canonical_len(value)?;
        }
        if total > MAX_ENVELOPE_UNSUPPORTED_BYTES {
            return Err(ModelError::Validation(format!(
                "unsupported payload of {total} bytes exceeds {MAX_ENVELOPE_UNSUPPORTED_BYTES}"
            )));
        }
        Ok(())
    }
}

fn validate_levels(levels: &crate::Levels) -> Result<(), ModelError> {
    for (name, channel) in [
        ("rgb", &levels.rgb),
        ("red", &levels.red),
        ("green", &levels.green),
        ("blue", &levels.blue),
    ] {
        for (key, value, min, max) in [
            ("inputBlack", channel.input_black, 0.0, 255.0),
            ("inputWhite", channel.input_white, 0.0, 255.0),
            ("outputBlack", channel.output_black, 0.0, 255.0),
            ("outputWhite", channel.output_white, 0.0, 255.0),
            ("midtone", channel.midtone, -1.0, 1.0),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(ModelError::Validation(format!(
                    "recipe.levels.{name}.{key} must be finite and in [{min}, {max}]"
                )));
            }
        }
        if channel.input_white - channel.input_black < 1.0
            || channel.output_white - channel.output_black < 1.0
        {
            return Err(ModelError::Validation(format!(
                "recipe.levels.{name}: white must exceed black by at least 1"
            )));
        }
    }
    Ok(())
}
