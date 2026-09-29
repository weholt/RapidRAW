//! Parsing, validated migrations and legacy imports.
//!
//! - `parse_envelope` accepts only the current schema version and projects
//!   unknown envelope fields into `unsupported` (nothing is silently dropped).
//! - `migrate_envelope` additionally imports legacy `.rrdata` documents
//!   (no `schemaVersion`) and rejects future schemas while preserving the
//!   original payload on the error.
//! - Lap legacy CSS-filter saturation is converted arithmetically: the legacy
//!   scale is 0..=200 with neutral 100, the recipe deviation scale is
//!   -100..=100 with neutral 0. Field names are never matched by similarity.

use crate::types::{MaskContainer, Recipe, RecipeEnvelope};
use crate::{MODEL_VERSION, ModelError, SCHEMA_VERSION};
use serde_json::Value;
use std::collections::BTreeMap;

/// Host-supplied identity fields the model cannot infer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeIdentity {
    pub engine_version: String,
    pub asset_id: String,
    pub variant_id: String,
    pub source_fingerprint: String,
}

/// Every serialized recipe key (legacy-compatible names).
pub const RECIPE_KEYS: &[&str] = &[
    "exposure",
    "brightness",
    "contrast",
    "highlights",
    "shadows",
    "whites",
    "blacks",
    "toneMapper",
    "levels",
    "vignetting",
    "curves",
    "pointCurves",
    "parametricCurve",
    "curveMode",
    "temperature",
    "tint",
    "vibrance",
    "saturation",
    "hue",
    "colorGrading",
    "hsl",
    "colorCalibration",
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
    "grainAmount",
    "grainSize",
    "grainRoughness",
    "vignetteAmount",
    "vignetteMidpoint",
    "vignetteRoundness",
    "vignetteFeather",
    "lutIntensity",
    "lutIsSceneReferred",
    "lutName",
    "lutPath",
    "lutSize",
    "lensBlurEnabled",
    "lensBlurAmount",
    "lensBlurDiffusion",
    "lensBlurShape",
    "lensBlurDepthMap",
    "lensBlurMinDepth",
    "lensBlurMaxDepth",
    "lensBlurMinFade",
    "lensBlurMaxFade",
    "rotation",
    "orientationSteps",
    "flipHorizontal",
    "flipVertical",
    "crop",
    "aspectRatio",
    "transformDistortion",
    "transformVertical",
    "transformHorizontal",
    "transformRotate",
    "transformAspect",
    "transformScale",
    "transformXOffset",
    "transformYOffset",
    "lensCorrectionMode",
    "lensMaker",
    "lensModel",
    "lensDistortionAmount",
    "lensVignetteAmount",
    "lensTcaAmount",
    "lensDistortionEnabled",
    "lensTcaEnabled",
    "lensVignetteEnabled",
    "lensDistortionParams",
    "lensProfile",
    "masks",
    "sectionVisibility",
    "sectionOrder",
];

const ENVELOPE_KEYS: &[&str] = &[
    "assetId",
    "decode",
    "engineVersion",
    "recipe",
    "resources",
    "revision",
    "schemaVersion",
    "sourceFingerprint",
    "unsupported",
    "variantId",
];

/// UI-only legacy keys that must never become recipe data.
const UI_ONLY_KEYS: &[&str] = &["showClipping", "aiPatches"];

const MASK_KEYS: &[&str] = &[
    "adjustments",
    "id",
    "invert",
    "name",
    "opacity",
    "subMasks",
    "visible",
];

const MASK_ADJUSTMENT_KEYS: &[&str] = &[
    "blacks",
    "brightness",
    "centré",
    "chromaticAberrationBlueYellow",
    "chromaticAberrationRedCyan",
    "clarity",
    "colorCalibration",
    "colorGrading",
    "colorNoiseReduction",
    "contrast",
    "curveMode",
    "curves",
    "dehaze",
    "exposure",
    "flareAmount",
    "glowAmount",
    "halationAmount",
    "highlights",
    "hsl",
    "hue",
    "lumaNoiseReduction",
    "parametricCurve",
    "pointCurves",
    "saturation",
    "sectionVisibility",
    "shadows",
    "sharpness",
    "sharpnessThreshold",
    "structure",
    "temperature",
    "tint",
    "vibrance",
    "whites",
];

const SUB_MASK_KEYS: &[&str] = &[
    "id",
    "invert",
    "mode",
    "name",
    "opacity",
    "parameters",
    "type",
    "visible",
];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MigrationReport {
    pub from: Option<u32>,
    pub applied: Vec<String>,
    pub excluded_ui_fields: Vec<String>,
    pub preserved_keys: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportReport {
    pub excluded_ui_fields: Vec<String>,
    pub preserved_keys: Vec<String>,
    pub preserved: BTreeMap<String, Value>,
}

fn split_object(
    value: &Value,
    known: &[&str],
) -> Result<(Value, Vec<(String, Value)>), ModelError> {
    let obj = value
        .as_object()
        .ok_or_else(|| ModelError::Validation("expected a JSON object".to_string()))?;
    let mut projected = serde_json::Map::new();
    let mut unknown = Vec::new();
    for (key, val) in obj {
        if known.contains(&key.as_str()) {
            projected.insert(key.clone(), val.clone());
        } else {
            unknown.push((key.clone(), val.clone()));
        }
    }
    Ok((Value::Object(projected), unknown))
}

fn insert_bounded(
    map: &mut BTreeMap<String, Value>,
    key: String,
    value: Value,
    max_bytes: usize,
) -> Result<(), ModelError> {
    let size = crate::canonical::canonical_bytes(&value)?.len();
    if size > max_bytes {
        return Err(ModelError::Validation(format!(
            "unsupported payload '{key}' of {size} bytes exceeds the {max_bytes}-byte bound"
        )));
    }
    map.insert(key, value);
    Ok(())
}

/// Parse a current-version envelope. Unknown envelope-level fields are moved
/// into `unsupported` with an `envelope.` prefix, then the whole envelope is
/// validated.
pub fn parse_envelope(json: &str) -> Result<RecipeEnvelope, ModelError> {
    let value: Value = serde_json::from_str(json)?;
    parse_envelope_value(value)
}

pub fn parse_envelope_value(value: Value) -> Result<RecipeEnvelope, ModelError> {
    let version = value
        .as_object()
        .and_then(|obj| obj.get("schemaVersion"))
        .and_then(Value::as_u64);
    match version {
        Some(v) if v > SCHEMA_VERSION as u64 => {
            return Err(ModelError::UnsupportedSchema {
                found: Some(v as u32),
                supported_max: SCHEMA_VERSION,
                preserved: value,
            });
        }
        Some(v) if v == SCHEMA_VERSION as u64 => {}
        Some(v) => {
            return Err(ModelError::Validation(format!(
                "schemaVersion {v} is not a valid version for this reader (supported: {SCHEMA_VERSION})"
            )));
        }
        None => {
            return Err(ModelError::Validation(
                "missing schemaVersion; use migrate_envelope for legacy documents".to_string(),
            ));
        }
    }

    let (known, unknown) = split_object(&value, ENVELOPE_KEYS)?;
    let mut envelope: RecipeEnvelope = serde_json::from_value(known)?;
    for (key, val) in unknown {
        insert_bounded(
            &mut envelope.unsupported,
            format!("envelope.{key}"),
            val,
            1024 * 1024,
        )?;
    }
    envelope.validate()?;
    Ok(envelope)
}

/// Migrate any supported document (current envelope, legacy `.rrdata`) into a
/// validated current envelope. Future schemas are rejected with the original
/// payload preserved.
pub fn migrate_envelope(
    json: &str,
    identity: &EnvelopeIdentity,
) -> Result<(RecipeEnvelope, MigrationReport), ModelError> {
    let value: Value = serde_json::from_str(json)?;
    let version = value
        .as_object()
        .and_then(|obj| obj.get("schemaVersion"))
        .and_then(Value::as_u64);

    if let Some(v) = version {
        if v > SCHEMA_VERSION as u64 {
            return Err(ModelError::UnsupportedSchema {
                found: Some(v as u32),
                supported_max: SCHEMA_VERSION,
                preserved: value,
            });
        }
        let envelope = parse_envelope_value(value)?;
        let report = MigrationReport {
            from: Some(v as u32),
            applied: Vec::new(),
            excluded_ui_fields: Vec::new(),
            preserved_keys: Vec::new(),
        };
        return Ok((envelope, report));
    }

    // Legacy path: either a `.rrdata` ImageMetadata document (has an
    // `adjustments` object) or a bare legacy adjustments object.
    let obj = value
        .as_object()
        .ok_or_else(|| ModelError::Validation("legacy document must be an object".to_string()))?;
    let (adjustments, legacy_metadata): (Value, Vec<(String, Value)>) = match obj.get("adjustments")
    {
        Some(adj) if adj.is_object() => {
            let metadata = obj
                .iter()
                .filter(|(k, _)| *k != "adjustments")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            (adj.clone(), metadata)
        }
        _ => (value.clone(), Vec::new()),
    };

    let (recipe, import_report) = recipe_from_legacy_adjustments(&adjustments)?;
    let mut envelope = RecipeEnvelope::new(
        &identity.engine_version,
        &identity.asset_id,
        &identity.variant_id,
        &identity.source_fingerprint,
    );
    envelope.recipe = recipe;
    for (key, val) in import_report.preserved {
        envelope.unsupported.insert(key, val);
    }
    for (key, val) in legacy_metadata {
        insert_bounded(
            &mut envelope.unsupported,
            format!("legacyMetadata.{key}"),
            val,
            1024 * 1024,
        )?;
    }
    envelope.validate()?;

    let mut preserved_keys: Vec<String> = import_report.preserved_keys;
    preserved_keys.extend(import_report.excluded_ui_fields.iter().cloned());
    let report = MigrationReport {
        from: None,
        applied: vec!["legacy-rrdata-v0".to_string()],
        excluded_ui_fields: import_report.excluded_ui_fields,
        preserved_keys,
    };
    Ok((envelope, report))
}

/// Import a legacy adjustments object into a validated semantic recipe.
/// Unknown and UI-only fields are reported with their payload preserved;
/// legacy pixel crops and inline LUT data are preserved for explicit host
/// conversion (dimensions / resource hashing), never dropped.
pub fn recipe_from_legacy_adjustments(
    legacy: &Value,
) -> Result<(Recipe, ImportReport), ModelError> {
    let obj = legacy.as_object().ok_or_else(|| {
        ModelError::Validation("legacy adjustments must be an object".to_string())
    })?;
    let mut projected = serde_json::Map::new();
    let mut report = ImportReport::default();

    for (key, val) in obj {
        if UI_ONLY_KEYS.contains(&key.as_str()) {
            report.excluded_ui_fields.push(key.clone());
            if key == "aiPatches" {
                let is_meaningful = match val {
                    Value::Null => false,
                    Value::Array(items) => !items.is_empty(),
                    _ => true,
                };
                if is_meaningful {
                    insert_bounded(
                        &mut report.preserved,
                        "legacy.aiPatches".to_string(),
                        val.clone(),
                        2 * 1024 * 1024,
                    )?;
                    report.preserved_keys.push("legacy.aiPatches".to_string());
                }
            }
            continue;
        }
        if key == "crop" {
            if !val.is_null() {
                insert_bounded(
                    &mut report.preserved,
                    "legacyAdjustments.crop".to_string(),
                    val.clone(),
                    64 * 1024,
                )?;
                report
                    .preserved_keys
                    .push("legacyAdjustments.crop".to_string());
            }
            continue;
        }
        if key == "lutData" {
            if !val.is_null() {
                insert_bounded(
                    &mut report.preserved,
                    "legacy.lutData".to_string(),
                    val.clone(),
                    4 * 1024 * 1024,
                )?;
                report.preserved_keys.push("legacy.lutData".to_string());
            }
            continue;
        }
        if RECIPE_KEYS.contains(&key.as_str()) {
            projected.insert(key.clone(), val.clone());
        } else {
            insert_bounded(
                &mut report.preserved,
                format!("legacyAdjustments.{key}"),
                val.clone(),
                1024 * 1024,
            )?;
            report
                .preserved_keys
                .push(format!("legacyAdjustments.{key}"));
        }
    }

    // Masks need nested splitting so unknown mask fields are preserved
    // instead of silently ignored by serde.
    if let Some(masks) = projected.get_mut("masks")
        && let Some(mask_list) = masks.as_array_mut()
    {
        for mask_value in mask_list.iter_mut() {
            *mask_value = import_mask(mask_value)?;
        }
    }

    let recipe: Recipe = serde_json::from_value(Value::Object(projected))?;
    recipe.validate()?;
    Ok((recipe, report))
}

fn import_mask(mask_value: &Value) -> Result<Value, ModelError> {
    if !mask_value.is_object() {
        return Ok(mask_value.clone());
    }
    let (known, unknown) = split_object(mask_value, MASK_KEYS)?;
    let mut projected = known
        .as_object()
        .cloned()
        .ok_or_else(|| ModelError::Validation("mask projection failed".to_string()))?;
    let mut unsupported = serde_json::Map::new();
    for (key, val) in unknown {
        unsupported.insert(key, val);
    }

    if let Some(adj) = projected.get_mut("adjustments")
        && adj.is_object()
    {
        let (adj_known, adj_unknown) = split_object(adj, MASK_ADJUSTMENT_KEYS)?;
        let mut adj_map = adj_known.as_object().cloned().ok_or_else(|| {
            ModelError::Validation("mask adjustments projection failed".to_string())
        })?;
        for (key, val) in adj_unknown {
            unsupported.insert(format!("adjustments.{key}"), val);
        }
        if let Some(section_visibility) = adj_map.get_mut("sectionVisibility")
            && section_visibility.is_object()
        {
            let (sv_known, sv_unknown) = split_object(
                section_visibility,
                &["basic", "curves", "color", "details", "effects"],
            )?;
            for (key, val) in sv_unknown {
                unsupported.insert(format!("adjustments.sectionVisibility.{key}"), val);
            }
            *section_visibility = sv_known;
        }
        *adj = Value::Object(adj_map);
    }

    if let Some(sub_masks) = projected.get_mut("subMasks")
        && let Some(list) = sub_masks.as_array_mut()
    {
        for (index, sub_mask) in list.iter_mut().enumerate() {
            if sub_mask.is_object() {
                let (sub_known, sub_unknown) = split_object(sub_mask, SUB_MASK_KEYS)?;
                for (key, val) in sub_unknown {
                    unsupported.insert(format!("subMasks.{index}.{key}"), val);
                }
                *sub_mask = sub_known;
            }
        }
    }

    let mut container: MaskContainer = serde_json::from_value(Value::Object(projected))?;
    container.unsupported = unsupported.into_iter().collect();
    serde_json::to_value(container).map_err(ModelError::Json)
}

pub const SUPPORTED_SCHEMA_MAX: u32 = SCHEMA_VERSION;

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if value.is_nan() {
        return min;
    }
    value.clamp(min, max)
}

/// Convert a Lap legacy CSS-filter saturation value (neutral 100, range
/// 0..=200) to the recipe deviation value (neutral 0, range -100..=100).
/// Inputs are clamped; NaN maps to neutral.
pub fn saturation_from_lap_legacy(legacy: f64) -> f64 {
    clamp(legacy, 0.0, 200.0) - 100.0
}

/// Convert a recipe saturation value back to the Lap legacy scale.
pub fn saturation_to_lap_legacy(recipe: f64) -> f64 {
    clamp(recipe, -100.0, 100.0) + 100.0
}

/// Default engine version recorded when the host does not supply one.
pub fn default_engine_version() -> String {
    MODEL_VERSION.to_string()
}
