//! Engine-side contract tests: the Rust backend consumes
//! `rapidraw-edit-model` as the single authority for recipe parsing,
//! validation, hashing and migration. These tests pin that consumption so the
//! application cannot silently regress to treating recipes as loose JSON.

use rapidraw_edit_model::types::{Recipe, RecipeEnvelope, ResourceAlgorithm, ResourceRef};
use rapidraw_edit_model::{
    EnvelopeIdentity, ModelError, SCHEMA_VERSION, migrate_envelope, parse_envelope, validate_recipe,
};

fn identity() -> EnvelopeIdentity {
    EnvelopeIdentity {
        engine_version: format!("RapidRAW-engine/{}", rapidraw_edit_model::MODEL_VERSION),
        asset_id: "asset-abc".to_string(),
        variant_id: "primary".to_string(),
        source_fingerprint: "f".repeat(64),
    }
}

#[test]
fn built_envelope_from_recipe_validates_and_hashes_deterministically() {
    let build = || {
        let mut envelope = RecipeEnvelope::new(
            &identity().engine_version,
            &identity().asset_id,
            &identity().variant_id,
            &identity().source_fingerprint,
        );
        envelope.recipe.exposure = 0.35;
        envelope.recipe.saturation = -8.0;
        envelope.recipe.section_visibility.curves = false;
        envelope.resources.insert(
            "lut/teal-orange".to_string(),
            ResourceRef {
                algorithm: ResourceAlgorithm::Sha256,
                digest: "a".repeat(64),
                size_bytes: Some(262_144),
            },
        );
        envelope
    };

    let a = build();
    let b = build();
    a.validate().unwrap();
    assert_eq!(a, b);
    assert_eq!(a.content_hash().unwrap(), b.content_hash().unwrap());
    assert_eq!(a.schema_version, SCHEMA_VERSION);

    let mut changed = build();
    changed.recipe.exposure = 0.5;
    assert_ne!(a.content_hash().unwrap(), changed.content_hash().unwrap());
}

#[test]
fn backend_rejects_invalid_recipes_instead_of_rendering_them() {
    let recipe = Recipe {
        saturation: 150.0,
        ..Recipe::default()
    };
    match validate_recipe(&recipe) {
        Err(ModelError::Validation(msg)) => assert!(msg.contains("saturation"), "{msg}"),
        other => panic!("expected validation error, got {other:?}"),
    }

    let recipe = Recipe {
        exposure: f64::NAN,
        ..Recipe::default()
    };
    assert!(validate_recipe(&recipe).is_err());
}

#[test]
fn backend_rejects_future_schemas_with_preserved_payload() {
    let json = format!(
        r#"{{"schemaVersion":999,"assetId":"asset-abc","variantId":"primary","revision":3,
            "engineVersion":"future","sourceFingerprint":"{}","decode":{{}},"recipe":{{}},
            "resources":{{}},"unsupported":{{}}}}"#,
        "0".repeat(64)
    );
    match parse_envelope(&json) {
        Err(ModelError::UnsupportedSchema {
            found,
            supported_max,
            preserved,
        }) => {
            assert_eq!(found, Some(999));
            assert_eq!(supported_max, SCHEMA_VERSION);
            assert_eq!(preserved["assetId"], "asset-abc");
        }
        other => panic!("expected future-schema rejection, got {other:?}"),
    }
}

#[test]
fn backend_imports_legacy_rrdata_documents() {
    let legacy = serde_json::json!({
        "version": 1,
        "adjustments": {
            "exposure": 0.25,
            "centré": 12,
            "showClipping": true
        }
    });
    let json = serde_json::to_string(&legacy).unwrap();
    let (envelope, report) = migrate_envelope(&json, &identity()).unwrap();
    assert_eq!(envelope.schema_version, SCHEMA_VERSION);
    assert_eq!(envelope.recipe.exposure, 0.25);
    assert_eq!(envelope.recipe.centre, 12.0);
    assert!(report.from.is_none());
    envelope.validate().unwrap();
}

#[test]
fn generated_default_fixture_matches_the_model() {
    let json = rapidraw_edit_model::contract::default_recipe_json();
    let recipe: Recipe = serde_json::from_str(&json).unwrap();
    assert_eq!(recipe, Recipe::default());
    recipe.validate().unwrap();
}
