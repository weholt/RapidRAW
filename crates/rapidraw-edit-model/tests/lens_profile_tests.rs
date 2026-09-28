//! Lens-profile provenance on the recipe (lap-d52 / TASK-502).
//!
//! The recipe records WHICH lens-correction profile produced the persisted
//! `lensDistortionParams`: identity (maker/model), version label, content
//! hash, and the portable `resource://lens/<sha256>` URI. The envelope's
//! resource map must carry the matching content-addressed entry so the hash
//! is part of the durable recipe and every derivative cache identity (Lap
//! `DerivativeIdentity` hashes the resource map; the canonical recipe hash
//! covers the profile ref). A recipe that references a profile without the
//! envelope entry is invalid: renders could never verify the coefficients.

use rapidraw_edit_model::{
    LensProfileRef, ModelError, RecipeEnvelope, ResourceAlgorithm, ResourceRef, sha256_hex,
};

const DIGEST: &str = "3d4f19c0e53cba5ee3f6d8ef1b74f0e00e1a3bdcb76ee5ff8adb330c4fca1e57";

fn profile_ref() -> LensProfileRef {
    LensProfileRef {
        uri: format!("resource://lens/{DIGEST}"),
        maker: "Tamron".to_string(),
        model: "17-28mm F2.8 Di III RXD".to_string(),
        version: "lensfun 2020-01-01".to_string(),
        sha256: DIGEST.to_string(),
    }
}

fn resource_entry() -> ResourceRef {
    ResourceRef {
        algorithm: ResourceAlgorithm::Sha256,
        digest: DIGEST.to_string(),
        size_bytes: Some(1024),
    }
}

fn envelope_with_profile() -> RecipeEnvelope {
    let mut envelope = RecipeEnvelope::default();
    envelope.recipe.lens_profile = Some(profile_ref());
    envelope
}

#[test]
fn lens_profile_ref_round_trips_and_changes_the_content_hash() {
    let mut envelope = envelope_with_profile();
    envelope
        .resources
        .insert(format!("lens/{DIGEST}"), resource_entry());
    envelope.validate().expect("a mapped profile ref validates");

    let without = RecipeEnvelope::default();
    let hash_with = envelope.content_hash().unwrap();
    let hash_without = without.content_hash().unwrap();
    assert_ne!(
        hash_with, hash_without,
        "the profile provenance must change the recipe content hash so cache identities split"
    );

    let json = serde_json::to_string(&envelope.recipe).unwrap();
    assert!(
        json.contains("lensProfile") && json.contains(&format!("resource://lens/{DIGEST}")),
        "serialization must be camelCase and carry the portable uri: {json}"
    );

    let parsed: RecipeEnvelope = serde_json::from_str(&serde_json::to_string(&envelope).unwrap())
        .expect("envelope round trip");
    assert_eq!(parsed, envelope);
}

#[test]
fn envelopes_without_a_lens_profile_field_still_parse() {
    let envelope = RecipeEnvelope::default();
    let json = serde_json::to_string(&envelope).unwrap();
    let stripped = json.replace(
        &format!(
            "\"lensProfile\":{}",
            serde_json::to_string(&profile_ref()).unwrap()
        ),
        "",
    );
    // The default envelope has `lensProfile: null`; remove the key entirely to
    // simulate an older document written before the field existed.
    let stripped = stripped.replace("\"lensProfile\":null,", "");
    let parsed: RecipeEnvelope = serde_json::from_str(&stripped).expect("older documents parse");
    assert!(parsed.recipe.lens_profile.is_none());
    envelope_with_profile()
        .recipe
        .lens_profile
        .as_ref()
        .unwrap();
    let _ = sha256_hex(b"x");
}

#[test]
fn malformed_lens_profile_refs_fail_validation() {
    let cases: Vec<(&str, LensProfileRef)> = vec![
        (
            "uri digest does not match sha256",
            LensProfileRef {
                uri: "resource://lens/1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                maker: "Tamron".to_string(),
                model: "17-28mm".to_string(),
                version: "v1".to_string(),
                sha256: DIGEST.to_string(),
            },
        ),
        (
            "non-hex digest",
            LensProfileRef {
                uri: format!("resource://lens/{}", "z".repeat(64)),
                maker: "Tamron".to_string(),
                model: "17-28mm".to_string(),
                version: "v1".to_string(),
                sha256: "z".repeat(64),
            },
        ),
        (
            "uppercase digest",
            LensProfileRef {
                uri: format!("resource://lens/{}", DIGEST.to_uppercase()),
                maker: "Tamron".to_string(),
                model: "17-28mm".to_string(),
                version: "v1".to_string(),
                sha256: DIGEST.to_uppercase(),
            },
        ),
        (
            "empty maker",
            LensProfileRef {
                uri: format!("resource://lens/{DIGEST}"),
                maker: String::new(),
                model: "17-28mm".to_string(),
                version: "v1".to_string(),
                sha256: DIGEST.to_string(),
            },
        ),
        (
            "empty version",
            LensProfileRef {
                uri: format!("resource://lens/{DIGEST}"),
                maker: "Tamron".to_string(),
                model: "17-28mm".to_string(),
                version: String::new(),
                sha256: DIGEST.to_string(),
            },
        ),
        (
            "non-resource scheme",
            LensProfileRef {
                uri: format!("file:///C:/lensfun/{DIGEST}.xml"),
                maker: "Tamron".to_string(),
                model: "17-28mm".to_string(),
                version: "v1".to_string(),
                sha256: DIGEST.to_string(),
            },
        ),
        (
            "overlong model",
            LensProfileRef {
                uri: format!("resource://lens/{DIGEST}"),
                maker: "Tamron".to_string(),
                model: "x".repeat(200),
                version: "v1".to_string(),
                sha256: DIGEST.to_string(),
            },
        ),
    ];
    for (detail, profile) in cases {
        let mut envelope = RecipeEnvelope::default();
        envelope.recipe.lens_profile = Some(profile);
        let err = envelope
            .validate()
            .expect_err(&format!("'{detail}' must fail validation"));
        match err {
            ModelError::Validation(message) => {
                assert!(
                    message.contains("lensProfile"),
                    "'{detail}' error must name the field: {message}"
                );
            }
            other => panic!("'{detail}' must be a validation error, got {other:?}"),
        }
    }
}

#[test]
fn recipe_referencing_a_profile_requires_the_envelope_resource_entry() {
    let mut envelope = envelope_with_profile();
    let err = envelope
        .validate()
        .expect_err("an unmapped profile ref must fail envelope validation");
    match err {
        ModelError::Validation(message) => {
            assert!(
                message.contains("lens/") && message.contains(DIGEST),
                "error must name the expected resource key: {message}"
            );
        }
        other => panic!("expected validation error, got {other:?}"),
    }

    envelope
        .resources
        .insert(format!("lens/{DIGEST}"), resource_entry());
    envelope.validate().expect("mapped profile validates");

    // A mapped entry whose digest disagrees with the recipe sha256 is invalid.
    let mut envelope = envelope_with_profile();
    envelope.resources.insert(
        format!("lens/{DIGEST}"),
        ResourceRef {
            algorithm: ResourceAlgorithm::Sha256,
            digest: "2222222222222222222222222222222222222222222222222222222222222222".to_string(),
            size_bytes: Some(10),
        },
    );
    let err = envelope
        .validate()
        .expect_err("digest mismatch between recipe and resource map must fail");
    assert!(
        matches!(err, ModelError::Validation(ref m) if m.contains("lensProfile")),
        "error must name the lens profile: {err:?}"
    );
}

#[test]
fn recipe_with_legacy_params_but_no_profile_stays_valid() {
    // Imported rrdata carries resolved coefficients without provenance; the
    // recipe stays valid (the render records the unknown provenance as a
    // limitation instead of failing).
    let mut envelope = RecipeEnvelope::default();
    envelope.recipe.lens_maker = Some("Canon".to_string());
    envelope.recipe.lens_model = Some("EF 50mm f/1.8".to_string());
    envelope.recipe.lens_distortion_params = Some(rapidraw_edit_model::LensDistortionParams {
        k1: -0.01,
        k2: 0.005,
        k3: 0.0,
        model: 0.0,
        tca_vr: 1.0,
        tca_vb: 1.0,
        vig_k1: 0.0,
        vig_k2: 0.0,
        vig_k3: 0.0,
    });
    envelope.validate().expect("legacy lens params stay valid");
}
