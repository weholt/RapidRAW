//! Behavior tests for the validated recipe model.
//!
//! Coverage: defaults enumeration, bounded finite validation, migration and
//! future-schema rejection, legacy `.rrdata` import with preserved payloads,
//! explicit Lap legacy saturation conversion, canonical serialization/hash
//! determinism, generated frontend contract consistency, and a seeded
//! property test.

use rapidraw_edit_model::descriptors::PARAM_DESCRIPTORS;
use rapidraw_edit_model::geometry::{LegacyPixelCrop, crop_to_normalized};
use rapidraw_edit_model::migrate::{
    EnvelopeIdentity, ImportReport, RECIPE_KEYS, migrate_envelope, parse_envelope,
    recipe_from_legacy_adjustments, saturation_from_lap_legacy, saturation_to_lap_legacy,
};
use rapidraw_edit_model::types::{
    ColorGrading, CropRect, CurveMode, CurvePoint, Curves, EffectiveDecodeSettings, HueSatLum,
    LensBlurShape, LensCorrectionMode, MaskContainer, MaskLocalAdjustments,
    ParametricCurveSettings, Recipe, RecipeEnvelope, ResourceAlgorithm, ResourceRef, SectionId,
    SectionVisibility, SubMask, SubMaskMode, ToneMapper,
};
use rapidraw_edit_model::{ModelError, SCHEMA_VERSION, canonical_bytes, contract};

fn identity() -> EnvelopeIdentity {
    EnvelopeIdentity {
        engine_version: "test-engine-1.0".to_string(),
        asset_id: "asset-123".to_string(),
        variant_id: "primary".to_string(),
        source_fingerprint: "a".repeat(64),
    }
}

fn base_envelope() -> RecipeEnvelope {
    RecipeEnvelope {
        engine_version: "test-engine-1.0".to_string(),
        asset_id: "asset-123".to_string(),
        variant_id: "primary".to_string(),
        source_fingerprint: "a".repeat(64),
        ..RecipeEnvelope::default()
    }
}

fn assert_validation_error(recipe: &Recipe, needle: &str) {
    match rapidraw_edit_model::validate_recipe(recipe) {
        Err(ModelError::Validation(msg)) => assert!(
            msg.contains(needle),
            "expected '{needle}' in validation error, got: {msg}"
        ),
        other => panic!("expected validation error mentioning '{needle}', got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Defaults and enumeration
// ---------------------------------------------------------------------------

#[test]
fn defaults_match_rapidraw_initial_adjustments() {
    let recipe = Recipe::default();
    let assert_eq_num = |actual: f64, expected: f64, what: &str| {
        assert_eq!(actual, expected, "{what}");
    };
    assert_eq_num(recipe.saturation, 0.0, "RapidRAW saturation default is 0");
    assert_eq_num(recipe.grain_size, 25.0, "grainSize");
    assert_eq_num(recipe.grain_roughness, 50.0, "grainRoughness");
    assert_eq_num(recipe.sharpness_threshold, 15.0, "sharpnessThreshold");
    assert_eq_num(recipe.lut_intensity, 100.0, "lutIntensity");
    assert_eq_num(recipe.vignette_midpoint, 50.0, "vignetteMidpoint");
    assert_eq_num(recipe.vignette_feather, 50.0, "vignetteFeather");
    assert_eq_num(recipe.lens_blur_amount, 40.0, "lensBlurAmount");
    assert_eq_num(recipe.lens_blur_min_depth, 20.0, "lensBlurMinDepth");
    assert_eq_num(recipe.lens_blur_max_depth, 100.0, "lensBlurMaxDepth");
    assert_eq_num(recipe.lens_blur_min_fade, 20.0, "lensBlurMinFade");
    assert_eq_num(recipe.lens_blur_max_fade, 20.0, "lensBlurMaxFade");
    assert_eq_num(recipe.lens_distortion_amount, 100.0, "lensDistortionAmount");
    assert_eq_num(recipe.lens_vignette_amount, 100.0, "lensVignetteAmount");
    assert_eq_num(recipe.lens_tca_amount, 100.0, "lensTcaAmount");
    assert_eq_num(recipe.transform_scale, 100.0, "transformScale");
    assert_eq_num(recipe.color_grading.blending, 50.0, "colorGrading.blending");
    assert_eq!(recipe.tone_mapper, ToneMapper::Basic);
    assert_eq!(recipe.curve_mode, CurveMode::Point);
    assert_eq!(recipe.lens_correction_mode, LensCorrectionMode::Manual);
    assert_eq!(recipe.lens_blur_shape, LensBlurShape::Circle);
    assert!(recipe.lens_distortion_enabled);
    assert!(recipe.lens_tca_enabled);
    assert!(recipe.lens_vignette_enabled);
    assert!(!recipe.lens_blur_enabled);
    assert_eq!(recipe.orientation_steps, 0);
    for channel in [
        &recipe.curves.luma,
        &recipe.curves.red,
        &recipe.curves.green,
        &recipe.curves.blue,
    ] {
        assert_eq!(channel.len(), 2);
        assert_eq!(channel[0], CurvePoint { x: 0.0, y: 0.0 });
        assert_eq!(channel[1], CurvePoint { x: 255.0, y: 255.0 });
    }
    for settings in [
        &recipe.parametric_curve.luma,
        &recipe.parametric_curve.red,
        &recipe.parametric_curve.green,
        &recipe.parametric_curve.blue,
    ] {
        assert_eq!(
            *settings,
            ParametricCurveSettings {
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
        );
    }
    assert_eq!(
        recipe.section_visibility,
        SectionVisibility {
            basic: true,
            curves: true,
            color: true,
            details: true,
            effects: true,
        }
    );
}

#[test]
fn default_recipe_validates() {
    Recipe::default().validate().unwrap();
    base_envelope().validate().unwrap();
}

#[test]
fn section_order_defaults_to_canonical_and_accepts_permutations_only() {
    let recipe = Recipe::default();
    assert_eq!(
        recipe.section_order,
        vec![
            SectionId::Basic,
            SectionId::Curves,
            SectionId::Color,
            SectionId::Details,
            SectionId::Effects
        ]
    );

    let mut permuted = recipe.clone();
    permuted.section_order = vec![
        SectionId::Color,
        SectionId::Basic,
        SectionId::Details,
        SectionId::Effects,
        SectionId::Curves,
    ];
    permuted.validate().unwrap();

    let mut duplicated = recipe.clone();
    duplicated.section_order = vec![SectionId::Basic; 5];
    assert_validation_error(&duplicated, "sectionOrder");

    let mut incomplete = recipe;
    incomplete.section_order = vec![SectionId::Basic, SectionId::Color];
    assert_validation_error(&incomplete, "sectionOrder");
}

#[test]
fn ui_only_fields_stay_outside_the_recipe() {
    let value = serde_json::to_value(Recipe::default()).unwrap();
    let obj = value.as_object().unwrap();
    assert!(!obj.contains_key("showClipping"));
    assert!(!obj.contains_key("aiPatches"));

    let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    let mut expected: Vec<&str> = RECIPE_KEYS.to_vec();
    expected.sort_unstable();
    assert_eq!(
        keys, expected,
        "serialized keys must match the known key list"
    );
}

#[test]
fn envelope_contains_all_required_contract_fields() {
    let value = serde_json::to_value(base_envelope()).unwrap();
    let obj = value.as_object().unwrap();
    for key in [
        "schemaVersion",
        "engineVersion",
        "assetId",
        "variantId",
        "revision",
        "sourceFingerprint",
        "decode",
        "recipe",
        "resources",
        "unsupported",
    ] {
        assert!(obj.contains_key(key), "envelope missing {key}");
    }
    assert_eq!(obj["schemaVersion"], SCHEMA_VERSION);
    let decode: EffectiveDecodeSettings = serde_json::from_value(obj["decode"].clone()).unwrap();
    assert_eq!(decode.highlight_compression, 2.5);
    assert_eq!(decode.raw_color_noise_reduction, 0.5);
    assert_eq!(decode.raw_sharpening, 0.35);
}

// ---------------------------------------------------------------------------
// Descriptor table consistency
// ---------------------------------------------------------------------------

#[test]
fn descriptor_table_is_consistent() {
    let default = serde_json::to_value(Recipe::default()).unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for d in PARAM_DESCRIPTORS {
        assert!(seen.insert(d.key), "duplicate descriptor key {}", d.key);
        assert!(d.min.is_finite() && d.max.is_finite() && d.step.is_finite());
        assert!(d.min < d.max, "{}: min must be < max", d.key);
        assert!(d.step > 0.0, "{}: step must be > 0", d.key);
        assert!(
            d.default >= d.min && d.default <= d.max,
            "{}: default outside bounds",
            d.key
        );
        let actual = default
            .get(d.key)
            .and_then(|v| v.as_f64())
            .unwrap_or_else(|| panic!("descriptor key {} missing from default recipe", d.key));
        assert_eq!(
            actual, d.default,
            "descriptor default for {} drifted from the recipe default",
            d.key
        );
    }
}

// ---------------------------------------------------------------------------
// Validation: finite and bounded
// ---------------------------------------------------------------------------

#[test]
fn rejects_non_finite_numbers() {
    let recipe = Recipe {
        exposure: f64::NAN,
        ..Recipe::default()
    };
    assert_validation_error(&recipe, "exposure");

    let recipe = Recipe {
        saturation: f64::INFINITY,
        ..Recipe::default()
    };
    assert_validation_error(&recipe, "saturation");

    let mut recipe = Recipe::default();
    recipe.hsl.blues.hue = f64::NAN;
    assert_validation_error(&recipe, "hsl");

    let mut recipe = Recipe::default();
    recipe.color_grading.balance = f64::NEG_INFINITY;
    assert_validation_error(&recipe, "colorGrading");
}

type RecipeMutation = fn(&mut Recipe);

#[test]
fn rejects_out_of_bounds_scalars() {
    let cases: &[(RecipeMutation, &str)] = &[
        (|r: &mut Recipe| r.saturation = 150.0, "saturation"),
        (|r: &mut Recipe| r.exposure = 6.0, "exposure"),
        (|r: &mut Recipe| r.transform_scale = 40.0, "transformScale"),
        (
            |r: &mut Recipe| r.transform_rotate = 90.0,
            "transformRotate",
        ),
        (
            |r: &mut Recipe| r.sharpness_threshold = 95.0,
            "sharpnessThreshold",
        ),
        (|r: &mut Recipe| r.rotation = 200.0, "rotation"),
        (|r: &mut Recipe| r.orientation_steps = 4, "orientationSteps"),
        (|r: &mut Recipe| r.lut_size = 4097, "lutSize"),
        (|r: &mut Recipe| r.hue = 181.0, "hue"),
        (
            |r: &mut Recipe| r.lens_distortion_amount = 201.0,
            "lensDistortionAmount",
        ),
    ];
    for (mutate, needle) in cases {
        let mut recipe = Recipe::default();
        mutate(&mut recipe);
        assert_validation_error(&recipe, needle);
    }
}

#[test]
fn rejects_out_of_bounds_nested_structures() {
    let mut recipe = Recipe::default();
    recipe.color_grading.blending = 150.0;
    assert_validation_error(&recipe, "colorGrading");

    let mut recipe = Recipe::default();
    recipe.parametric_curve.luma.split1 = 120.0;
    assert_validation_error(&recipe, "parametricCurve");

    let mut recipe = Recipe::default();
    recipe.color_calibration.red_hue = 101.0;
    assert_validation_error(&recipe, "colorCalibration");
}

#[test]
fn rejects_invalid_curves() {
    let build = |points: Vec<CurvePoint>| -> Recipe {
        let mut recipe = Recipe::default();
        recipe.curves = Curves {
            luma: points,
            ..Curves::identity()
        };
        recipe
    };
    assert_validation_error(
        &build(vec![
            CurvePoint { x: 0.0, y: 0.0 },
            CurvePoint { x: 100.0, y: 10.0 },
            CurvePoint { x: 50.0, y: 20.0 },
            CurvePoint { x: 255.0, y: 255.0 },
        ]),
        "curves",
    );
    assert_validation_error(
        &build(vec![
            CurvePoint { x: 0.0, y: 0.0 },
            CurvePoint { x: 300.0, y: 10.0 },
        ]),
        "curves",
    );
    assert_validation_error(
        &build(vec![
            CurvePoint { x: 10.0, y: 0.0 },
            CurvePoint { x: 255.0, y: 255.0 },
        ]),
        "curves",
    );
    let too_many = (0..=32)
        .map(|i| CurvePoint {
            x: i as f64 * 8.0,
            y: 0.0,
        })
        .collect();
    assert_validation_error(&build(too_many), "curves");
    let mut recipe = Recipe::default();
    recipe.curves.luma[1].y = f64::NAN;
    assert_validation_error(&recipe, "curves");
}

#[test]
fn rejects_invalid_masks_and_sub_masks() {
    let mut recipe = Recipe::default();
    recipe.masks = (0..33)
        .map(|i| MaskContainer {
            id: format!("mask-{i}"),
            ..MaskContainer::default()
        })
        .collect();
    assert_validation_error(&recipe, "masks");

    let mut recipe = Recipe::default();
    recipe.masks = vec![MaskContainer {
        id: "mask-1".to_string(),
        opacity: f64::NAN,
        ..MaskContainer::default()
    }];
    assert_validation_error(&recipe, "masks");

    let mut recipe = Recipe::default();
    recipe.masks = vec![MaskContainer {
        id: "mask-1".to_string(),
        sub_masks: (0..17)
            .map(|i| SubMask {
                id: format!("sub-{i}"),
                kind: "brush".to_string(),
                ..SubMask::default()
            })
            .collect(),
        ..MaskContainer::default()
    }];
    assert_validation_error(&recipe, "subMasks");

    let big_parameters = serde_json::json!({ "strokes": "x".repeat(80_000) });
    let mut recipe = Recipe::default();
    recipe.masks = vec![MaskContainer {
        id: "mask-1".to_string(),
        sub_masks: vec![SubMask {
            id: "sub-1".to_string(),
            kind: "brush".to_string(),
            parameters: big_parameters,
            ..SubMask::default()
        }],
        ..MaskContainer::default()
    }];
    assert_validation_error(&recipe, "subMasks[0].parameters");

    let mut recipe = Recipe::default();
    recipe.masks = vec![MaskContainer {
        id: "mask-1".to_string(),
        adjustments: MaskLocalAdjustments {
            exposure: 9.0,
            ..MaskLocalAdjustments::default()
        },
        ..MaskContainer::default()
    }];
    assert_validation_error(&recipe, "masks");
}

#[test]
fn rejects_invalid_crop_and_aspect_ratio() {
    let mut recipe = Recipe::default();
    recipe.crop = Some(CropRect {
        x: 0.5,
        y: 0.0,
        width: 0.6,
        height: 1.0,
    });
    assert_validation_error(&recipe, "crop");

    let mut recipe = Recipe::default();
    recipe.crop = Some(CropRect {
        x: 0.1,
        y: 0.1,
        width: 0.0,
        height: 0.5,
    });
    assert_validation_error(&recipe, "crop");

    let mut recipe = Recipe::default();
    recipe.aspect_ratio = Some(0.0);
    assert_validation_error(&recipe, "aspectRatio");

    let mut recipe = Recipe::default();
    recipe.aspect_ratio = Some(1000.0);
    assert_validation_error(&recipe, "aspectRatio");
}

#[test]
fn rejects_invalid_envelope_identity_and_resources() {
    for mutate in [
        |e: &mut RecipeEnvelope| e.asset_id = String::new(),
        |e: &mut RecipeEnvelope| e.variant_id = String::new(),
        |e: &mut RecipeEnvelope| e.engine_version = String::new(),
        |e: &mut RecipeEnvelope| e.asset_id = "a".repeat(300),
        |e: &mut RecipeEnvelope| e.revision = 0,
        |e: &mut RecipeEnvelope| e.source_fingerprint = "a".repeat(63),
        |e: &mut RecipeEnvelope| e.source_fingerprint = "A".repeat(64),
        |e: &mut RecipeEnvelope| {
            e.source_fingerprint = "g".repeat(64);
        },
    ] {
        let mut envelope = base_envelope();
        mutate(&mut envelope);
        assert!(
            envelope.validate().is_err(),
            "expected rejection for {:?}",
            envelope
        );
    }

    let mut envelope = base_envelope();
    envelope.resources.insert(
        "lut/main".to_string(),
        ResourceRef {
            algorithm: ResourceAlgorithm::Sha256,
            digest: "nothex".to_string(),
            size_bytes: Some(10),
        },
    );
    assert!(envelope.validate().is_err());

    let mut envelope = base_envelope();
    envelope.resources.insert(
        "lut/main".to_string(),
        ResourceRef {
            algorithm: ResourceAlgorithm::Sha256,
            digest: "a".repeat(64),
            size_bytes: Some(1_073_741_825),
        },
    );
    assert!(envelope.validate().is_err());
}

type DecodeMutation = fn(&mut EffectiveDecodeSettings);

#[test]
fn rejects_invalid_decode_settings() {
    let cases: &[(DecodeMutation, &str)] = &[
        (
            |d: &mut EffectiveDecodeSettings| d.highlight_compression = 0.5,
            "highlightCompression",
        ),
        (
            |d: &mut EffectiveDecodeSettings| d.highlight_compression = 100.0,
            "highlightCompression",
        ),
        (
            |d: &mut EffectiveDecodeSettings| d.raw_color_noise_reduction = 1.5,
            "rawColorNoiseReduction",
        ),
        (
            |d: &mut EffectiveDecodeSettings| d.raw_sharpening = -0.1,
            "rawSharpening",
        ),
    ];
    for (mutate, needle) in cases {
        let mut envelope = base_envelope();
        mutate(&mut envelope.decode);
        match envelope.validate() {
            Err(ModelError::Validation(msg)) => assert!(msg.contains(needle), "{msg}"),
            other => panic!("expected validation error for {needle}, got {other:?}"),
        }
    }
}

#[test]
fn bounds_unsupported_payloads() {
    let mut envelope = base_envelope();
    for i in 0..65 {
        envelope
            .unsupported
            .insert(format!("envelope.extra{i}"), serde_json::json!(i));
    }
    assert!(envelope.validate().is_err());

    let mut envelope = base_envelope();
    envelope.unsupported.insert(
        "envelope.big".to_string(),
        serde_json::json!({ "blob": "x".repeat(3_000_000) }),
    );
    assert!(envelope.validate().is_err());
}

// ---------------------------------------------------------------------------
// Migration and legacy import
// ---------------------------------------------------------------------------

#[test]
fn rejects_future_schemas_and_preserves_payload() {
    let mut envelope = base_envelope();
    envelope.schema_version = 999;
    let json = serde_json::to_string(&envelope).unwrap();

    let err = parse_envelope(&json).unwrap_err();
    match &err {
        ModelError::UnsupportedSchema {
            found,
            supported_max,
            preserved,
        } => {
            assert_eq!(*found, Some(999));
            assert_eq!(*supported_max, SCHEMA_VERSION);
            assert_eq!(
                preserved.get("schemaVersion"),
                Some(&serde_json::json!(999))
            );
            assert_eq!(
                preserved.get("assetId"),
                Some(&serde_json::json!("asset-123"))
            );
        }
        other => panic!("expected UnsupportedSchema, got {other:?}"),
    }
    assert!(err.to_string().contains("preserved"));

    let migrated = migrate_envelope(&json, &identity());
    match migrated {
        Err(ModelError::UnsupportedSchema { found, .. }) => assert_eq!(found, Some(999)),
        other => panic!("expected future-schema rejection, got {other:?}"),
    }
}

#[test]
fn current_schema_round_trips_without_changes() {
    let mut envelope = base_envelope();
    envelope.recipe.exposure = 0.35;
    envelope.recipe.saturation = -10.0;
    let json = serde_json::to_string(&envelope).unwrap();

    let parsed = parse_envelope(&json).unwrap();
    assert_eq!(parsed, envelope);

    let (migrated, report) = migrate_envelope(&json, &identity()).unwrap();
    assert_eq!(report.from, Some(1));
    assert!(report.applied.is_empty());
    assert_eq!(migrated, envelope);
}

#[test]
fn imports_legacy_rrdata_adjustments() {
    let legacy = serde_json::json!({
        "version": 1,
        "adjustments": {
            "exposure": 0.4,
            "contrast": 15,
            "saturation": 0,
            "centré": -20,
            "grainRoughness": 75,
            "toneMapper": "agx",
            "showClipping": true,
            "aiPatches": [{ "id": "p1", "prompt": "remove wires" }],
            "someFutureField": { "a": 1 },
            "masks": [{
                "id": "m1",
                "name": "Mask 1",
                "invert": false,
                "visible": true,
                "opacity": 80,
                "subMasks": [{
                    "id": "s1",
                    "type": "radial",
                    "mode": "additive",
                    "invert": false,
                    "visible": true,
                    "opacity": 100,
                    "parameters": { "x": 100, "y": 100, "width": 50, "height": 50 },
                    "brandNewSubMaskField": true
                }],
                "adjustments": {
                    "exposure": 0.2,
                    "lumaNoiseReduction": -50,
                    "notYetKnown": 7
                },
                "maskLevelExtra": "keep"
            }],
            "crop": { "unit": "px", "x": 0, "y": 0, "width": 1000, "height": 500 }
        }
    });

    let (recipe, report) = recipe_from_legacy_adjustments(&legacy["adjustments"]).unwrap();
    assert_eq!(recipe.exposure, 0.4);
    assert_eq!(recipe.contrast, 15.0);
    assert_eq!(recipe.saturation, 0.0);
    assert_eq!(recipe.centre, -20.0);
    assert_eq!(recipe.grain_roughness, 75.0);
    assert_eq!(recipe.tone_mapper, ToneMapper::Agx);
    assert_eq!(
        recipe.grain_size, 25.0,
        "missing keys fall back to defaults"
    );
    assert!(recipe.masks.len() == 1);
    let mask = &recipe.masks[0];
    assert_eq!(mask.opacity, 80.0);
    assert_eq!(mask.adjustments.exposure, 0.2);
    assert_eq!(mask.adjustments.luma_noise_reduction, -50.0);
    assert_eq!(mask.sub_masks[0].kind, "radial");
    assert_eq!(
        mask.sub_masks[0].parameters["x"],
        serde_json::json!(100),
        "opaque sub-mask payload is preserved"
    );
    assert_eq!(
        mask.unsupported.get("subMasks.0.brandNewSubMaskField"),
        Some(&serde_json::json!(true)),
        "unknown sub-mask fields are preserved inside the mask"
    );
    assert_eq!(
        mask.unsupported.get("adjustments.notYetKnown"),
        Some(&serde_json::json!(7))
    );
    assert_eq!(
        mask.unsupported.get("maskLevelExtra"),
        Some(&serde_json::json!("keep"))
    );

    assert!(
        report
            .excluded_ui_fields
            .contains(&"showClipping".to_string())
    );
    assert!(report.excluded_ui_fields.contains(&"aiPatches".to_string()));
    assert!(
        report
            .preserved
            .contains_key("legacyAdjustments.someFutureField"),
        "unknown fields are preserved, not dropped"
    );
    assert!(
        report.preserved.contains_key("legacyAdjustments.crop"),
        "legacy pixel crops need explicit dimension conversion and are preserved"
    );
    assert!(recipe.crop.is_none());

    recipe.validate().unwrap();
}

#[test]
fn migrates_full_legacy_rrdata_document() {
    let legacy = serde_json::json!({
        "version": 1,
        "filename": "photo.CR2",
        "adjustments": {
            "exposure": 0.25,
            "lutData": "base64Payload".repeat(4),
            "lutIntensity": 60
        }
    });
    let json = serde_json::to_string(&legacy).unwrap();
    let (envelope, report) = migrate_envelope(&json, &identity()).unwrap();
    assert_eq!(report.from, None);
    assert_eq!(envelope.schema_version, SCHEMA_VERSION);
    assert_eq!(envelope.asset_id, "asset-123");
    assert_eq!(envelope.recipe.exposure, 0.25);
    assert_eq!(envelope.recipe.lut_intensity, 60.0);
    assert!(
        envelope.unsupported.contains_key("legacy.lutData"),
        "inline LUT data is preserved for explicit resource registration"
    );
    assert!(envelope.unsupported.contains_key("legacyMetadata.filename"));
    envelope.validate().unwrap();
}

#[test]
fn lap_legacy_saturation_is_converted_arithmetically() {
    // Lap legacy CSS-filter scale: 100 is neutral. RapidRAW recipe: 0 is
    // neutral. The conversion is arithmetic, never name matching.
    assert_eq!(saturation_from_lap_legacy(100.0), 0.0);
    assert_eq!(saturation_from_lap_legacy(0.0), -100.0);
    assert_eq!(saturation_from_lap_legacy(200.0), 100.0);
    assert_eq!(saturation_to_lap_legacy(0.0), 100.0);
    assert_eq!(saturation_to_lap_legacy(-100.0), 0.0);
    assert_eq!(saturation_to_lap_legacy(100.0), 200.0);

    for recipe_value in [-100.0, -37.5, 0.0, 42.0, 100.0] {
        let legacy = saturation_to_lap_legacy(recipe_value);
        assert_eq!(saturation_from_lap_legacy(legacy), recipe_value);
    }

    // Defaults agree: both hosts' neutral values map to recipe neutral.
    assert_eq!(
        saturation_from_lap_legacy(100.0),
        Recipe::default().saturation
    );
    // Out-of-range legacy inputs clamp instead of producing out-of-bounds recipes.
    assert_eq!(saturation_from_lap_legacy(500.0), 100.0);
    assert_eq!(saturation_from_lap_legacy(-500.0), -100.0);
    assert!(saturation_from_lap_legacy(f64::NAN).is_finite());
}

#[test]
fn saturation_descriptors_use_rapidraw_semantics() {
    let saturation = PARAM_DESCRIPTORS
        .iter()
        .find(|d| d.key == "saturation")
        .unwrap();
    assert_eq!(saturation.default, 0.0);
    assert_eq!(saturation.min, -100.0);
    assert_eq!(saturation.max, 100.0);
}

// ---------------------------------------------------------------------------
// Canonical serialization and hashing
// ---------------------------------------------------------------------------

#[test]
fn canonical_hash_is_independent_of_input_key_order() {
    let mut envelope = base_envelope();
    envelope.recipe.exposure = 0.5;
    let a = serde_json::to_string(&envelope).unwrap();
    let reordered_json = format!(
        "{{\"recipe\":{{\"saturation\":0.0,\"exposure\":0.5}},\"assetId\":\"{}\",\"variantId\":\"{}\",\"revision\":{},\"schemaVersion\":{},\"engineVersion\":\"{}\",\"sourceFingerprint\":\"{}\",\"decode\":{{}},\"resources\":{{}},\"unsupported\":{{}}}}",
        envelope.asset_id,
        envelope.variant_id,
        envelope.revision,
        envelope.schema_version,
        envelope.engine_version,
        envelope.source_fingerprint,
    );
    let parsed_a = parse_envelope(&a).unwrap();
    let parsed_b = parse_envelope(&reordered_json).unwrap();
    assert_eq!(
        parsed_a.content_hash().unwrap(),
        parsed_b.content_hash().unwrap()
    );
}

#[test]
fn canonical_hash_detects_any_change() {
    let a = base_envelope();
    let b = {
        let mut b = a.clone();
        b.recipe.exposure = 0.01;
        b
    };
    assert_ne!(a.content_hash().unwrap(), b.content_hash().unwrap());
    let c = {
        let mut c = a.clone();
        c.revision += 1;
        c
    };
    assert_ne!(a.content_hash().unwrap(), c.content_hash().unwrap());
}

#[test]
fn canonical_json_is_sorted_compact_and_idempotent() {
    let envelope = base_envelope();
    let bytes = envelope.to_canonical_json().unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(
        !text.contains('\n') && !text.contains(": "),
        "canonical JSON must be compact"
    );
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    assert_eq!(
        canonical_bytes(&value).unwrap(),
        bytes,
        "canonicalization is idempotent"
    );
    assert_eq!(
        text.find("{\"assetId\""),
        Some(0),
        "keys are lexicographically sorted"
    );
}

#[test]
fn parse_reserializes_to_identical_canonical_bytes() {
    let mut envelope = base_envelope();
    envelope.recipe.exposure = 0.3;
    let json = serde_json::to_string(&envelope).unwrap();
    let parsed = parse_envelope(&json).unwrap();
    assert_eq!(parsed, envelope);
    assert_eq!(
        parsed.to_canonical_json().unwrap(),
        envelope.to_canonical_json().unwrap()
    );
}

// ---------------------------------------------------------------------------
// Property test (seeded, deterministic)
// ---------------------------------------------------------------------------

struct Lcg(u64);

impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    fn next_f64(&mut self, min: f64, max: f64) -> f64 {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        min + unit * (max - min)
    }
}

#[test]
fn randomized_in_bounds_recipes_validate_and_hash_stably() {
    let mut rng = Lcg(0x5e30bcbb);
    for _ in 0..200 {
        let mut recipe = Recipe::default();
        for d in PARAM_DESCRIPTORS {
            let value = rng.next_f64(d.min, d.max);
            let obj = serde_json::to_value(&recipe).unwrap();
            let mut obj = obj;
            obj[d.key] = serde_json::json!(value);
            recipe = serde_json::from_value(obj).unwrap();
        }
        recipe.validate().unwrap();

        let mut envelope = base_envelope();
        envelope.recipe = recipe;
        let hash_a = envelope.content_hash().unwrap();
        let json = serde_json::to_string(&envelope).unwrap();
        let reparsed = parse_envelope(&json).unwrap();
        assert_eq!(reparsed, envelope);
        assert_eq!(reparsed.content_hash().unwrap(), hash_a);
    }
}

// ---------------------------------------------------------------------------
// Generated frontend contract
// ---------------------------------------------------------------------------

#[test]
fn generated_typescript_exposes_the_contract() {
    let ts = contract::generate_typescript();
    assert!(ts.contains("export const RECIPE_SCHEMA_VERSION = 1;"));
    assert!(ts.contains("CANONICAL_SECTION_ORDER"));
    assert!(ts.contains("export interface Recipe"));
    assert!(ts.contains("export const DEFAULT_RECIPE: Recipe"));
    assert!(ts.contains("RECIPE_PARAM_RANGES"));
    assert!(ts.contains("saturationFromLapLegacy"));
    assert!(
        !ts.contains("showClipping"),
        "UI-only fields must stay out of the contract"
    );
    assert!(
        !ts.contains("aiPatches"),
        "UI-only fields must stay out of the contract"
    );
}

#[test]
fn generated_default_recipe_json_is_canonical_and_valid() {
    let json = contract::default_recipe_json();
    let recipe: Recipe = serde_json::from_str(&json).unwrap();
    assert_eq!(recipe, Recipe::default());
    assert!(
        !json.contains(char::is_whitespace),
        "fixture must be canonical JSON"
    );
    recipe.validate().unwrap();
}

#[test]
fn committed_generated_files_match_the_generator() {
    let gen_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("gen");
    let ts = std::fs::read_to_string(gen_dir.join("recipe.ts"))
        .expect("gen/recipe.ts must exist; run the update_generated_files test");
    assert_eq!(
        ts,
        contract::generate_typescript(),
        "gen/recipe.ts is stale"
    );
    let json = std::fs::read_to_string(gen_dir.join("default-recipe.json"))
        .expect("gen/default-recipe.json must exist; run the update_generated_files test");
    assert_eq!(
        json,
        contract::default_recipe_json(),
        "gen/default-recipe.json is stale"
    );
}

#[test]
fn crop_conversion_round_trip_matches_oriented_frame() {
    let crop = crop_to_normalized(
        LegacyPixelCrop {
            x: 100.0,
            y: 50.0,
            width: 500.0,
            height: 250.0,
        },
        1000.0,
        500.0,
    )
    .unwrap();
    assert_eq!(
        crop,
        CropRect {
            x: 0.1,
            y: 0.1,
            width: 0.5,
            height: 0.5
        }
    );
}

#[test]
fn import_report_default_is_empty() {
    let report = ImportReport::default();
    assert!(report.excluded_ui_fields.is_empty());
    assert!(report.preserved_keys.is_empty());
    assert!(report.preserved.is_empty());
}

#[test]
fn color_grading_and_hsl_defaults_are_neutral() {
    assert_eq!(
        ColorGrading::default(),
        ColorGrading {
            balance: 0.0,
            blending: 50.0,
            global: HueSatLum::default(),
            shadows: HueSatLum::default(),
            midtones: HueSatLum::default(),
            highlights: HueSatLum::default(),
        }
    );
    assert_eq!(SubMaskMode::default(), SubMaskMode::Additive);
}
