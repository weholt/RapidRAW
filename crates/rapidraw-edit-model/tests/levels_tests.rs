use rapidraw_edit_model::{Recipe, validate_recipe};
use serde_json::json;

#[test]
fn levels_old_recipes_default_to_independent_neutral_channels() {
    let recipe: Recipe = serde_json::from_value(json!({"exposure": 1.25})).unwrap();
    let value = serde_json::to_value(&recipe).unwrap();
    assert_eq!(value["levels"]["enabled"], true);
    for channel in ["rgb", "red", "green", "blue"] {
        assert_eq!(
            value["levels"][channel],
            json!({"inputBlack":0.0,"inputWhite":255.0,"outputBlack":0.0,"outputWhite":255.0,"midtone":0.0})
        );
    }
    assert_eq!(recipe.exposure, 1.25);
    validate_recipe(&recipe).unwrap();
}

#[test]
fn levels_round_trip_without_mutating_existing_curves() {
    let mut value = serde_json::to_value(Recipe::default()).unwrap();
    value["levels"] = json!({"enabled":false,"red":{"inputBlack":12.0,"inputWhite":230.0,"outputBlack":8.0,"outputWhite":244.0,"midtone":0.3}});
    let recipe: Recipe = serde_json::from_value(value.clone()).unwrap();
    validate_recipe(&recipe).unwrap();
    let restored = serde_json::to_value(&recipe).unwrap();
    assert_eq!(restored["levels"]["red"], value["levels"]["red"]);
    assert_eq!(restored["levels"]["enabled"], false);
    assert_eq!(restored["curves"], value["curves"]);
}

#[test]
fn levels_reject_invalid_endpoints_even_when_bypassed() {
    for channel in ["rgb", "red", "green", "blue"] {
        for invalid in [
            json!({"inputBlack":255}),
            json!({"inputWhite":0}),
            json!({"inputBlack":100,"inputWhite":100.5}),
            json!({"outputBlack":256}),
            json!({"outputWhite":0}),
            json!({"midtone":1.1}),
            json!({"inputBlack":-1}),
        ] {
            let mut value = serde_json::to_value(Recipe::default()).unwrap();
            value["levels"] = json!({"enabled":false});
            value["levels"][channel] = invalid;
            let recipe: Recipe = serde_json::from_value(value).unwrap();
            assert!(
                validate_recipe(&recipe).is_err(),
                "invalid {channel} levels accepted"
            );
        }
    }
}

#[test]
fn levels_reject_nonfinite_and_participate_in_canonical_persistence() {
    use rapidraw_edit_model::{RecipeEnvelope, parse_envelope};
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut recipe = Recipe::default();
        recipe.levels.green.input_white = invalid;
        assert!(
            validate_recipe(&recipe)
                .unwrap_err()
                .to_string()
                .contains("levels.green.inputWhite")
        );
        recipe.levels.green.input_white = 255.0;
        recipe.levels.blue.midtone = invalid;
        assert!(
            validate_recipe(&recipe)
                .unwrap_err()
                .to_string()
                .contains("levels.blue.midtone")
        );
    }
    let mut envelope = RecipeEnvelope {
        engine_version: "levels-test".into(),
        asset_id: "asset".into(),
        variant_id: "default".into(),
        source_fingerprint: "a".repeat(64),
        ..RecipeEnvelope::default()
    };
    let neutral_hash = envelope.content_hash().unwrap();
    envelope.recipe.levels.blue.input_white = 210.0;
    let hash = envelope.content_hash().unwrap();
    assert_ne!(hash, neutral_hash);
    let restored =
        parse_envelope(&String::from_utf8(envelope.to_canonical_json().unwrap()).unwrap()).unwrap();
    assert_eq!(restored.recipe.levels, envelope.recipe.levels);
    assert_eq!(restored.content_hash().unwrap(), hash);
    assert!(restored.unsupported.is_empty());
}
