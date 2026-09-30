use rapidraw_edit_model::{Recipe, RecipeEnvelope, parse_envelope, validate_recipe};
use serde_json::json;

#[test]
fn vignetting_defaults_round_trip_and_validation() {
    let legacy: Recipe = serde_json::from_value(json!({"vignetteAmount":-35})).unwrap();
    let v = serde_json::to_value(&legacy).unwrap();
    assert_eq!(
        v["vignetting"],
        json!({"enabled":true,"amount":0.0,"method":"ellipticOnCrop"})
    );
    assert_eq!(legacy.vignette_amount, -35.0);
    for method in ["ellipticOnCrop", "circularOnCrop", "circular"] {
        let mut env = RecipeEnvelope {
            engine_version: "test".into(),
            asset_id: "asset".into(),
            variant_id: "default".into(),
            source_fingerprint: "a".repeat(64),
            ..RecipeEnvelope::default()
        };
        let hash = env.content_hash().unwrap();
        env.recipe = serde_json::from_value(
            json!({"vignetting":{"enabled":false,"amount":-2.5,"method":method}}),
        )
        .unwrap();
        validate_recipe(&env.recipe).unwrap();
        let restored =
            parse_envelope(&String::from_utf8(env.to_canonical_json().unwrap()).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&restored.recipe).unwrap()["vignetting"],
            json!({"enabled":false,"amount":-2.5,"method":method})
        );
        assert_ne!(restored.content_hash().unwrap(), hash);
        assert!(restored.unsupported.is_empty());
    }
}

#[test]
fn vignetting_rejects_bad_amount_and_method() {
    for value in [
        json!({"amount":4.1}),
        json!({"amount":-4.1}),
        json!({"method":"unknown"}),
    ] {
        let parsed = serde_json::from_value::<Recipe>(json!({"vignetting":value}));
        assert!(parsed.is_err() || validate_recipe(&parsed.unwrap()).is_err());
    }
}

#[test]
fn vignetting_rejects_nonfinite_even_bypassed() {
    for amount in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut recipe = Recipe::default();
        recipe.vignetting.enabled = false;
        recipe.vignetting.amount = amount;
        assert!(validate_recipe(&recipe).is_err());
    }
}
