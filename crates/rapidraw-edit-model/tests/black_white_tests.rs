use rapidraw_edit_model::Recipe;

#[test]
fn black_white_conversion_is_independent_and_backward_compatible() {
    let original = Recipe::default();
    assert!(!original.black_white_enabled);
    assert_eq!(original.black_white_mix, [0.0; 8]);

    let mut edited = original.clone();
    edited.saturation = 35.0;
    edited.black_white_enabled = true;
    edited.black_white_mix[0] = 40.0;
    edited.validate().unwrap();
    let restored: Recipe = serde_json::from_value(serde_json::to_value(&edited).unwrap()).unwrap();
    assert!(restored.black_white_enabled);
    assert_eq!(restored.black_white_mix[0], 40.0);
    assert_eq!(restored.saturation, 35.0);

    let mut legacy = serde_json::to_value(&original).unwrap();
    legacy.as_object_mut().unwrap().remove("blackWhiteEnabled");
    legacy.as_object_mut().unwrap().remove("blackWhiteMix");
    let migrated: Recipe = serde_json::from_value(legacy).unwrap();
    assert!(!migrated.black_white_enabled);
    assert_eq!(migrated.black_white_mix, [0.0; 8]);
}

#[test]
fn black_white_mix_rejects_out_of_range_values() {
    let mut recipe = Recipe::default();
    recipe.black_white_mix[2] = 101.0;
    assert!(recipe.validate().is_err());
}
