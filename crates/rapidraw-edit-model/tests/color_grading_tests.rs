use rapidraw_edit_model::{MaskContainer, Recipe, validate_recipe};

#[test]
fn grading_accepts_full_circle_and_round_trips_global_and_mask_values() {
    let mut recipe = Recipe::default();
    recipe.color_grading.global.hue = 360.0;
    recipe.color_grading.shadows.hue = 240.0;
    recipe.color_grading.midtones.hue = 180.0;
    recipe.color_grading.highlights.hue = 300.0;
    let mut mask = MaskContainer {
        id: "grading-mask".into(),
        ..MaskContainer::default()
    };
    mask.adjustments.color_grading = recipe.color_grading.clone();
    recipe.masks.push(mask);
    validate_recipe(&recipe).expect("grading uses absolute hue degrees, not HSL offsets");
    let bytes = serde_json::to_vec(&recipe).unwrap();
    let restored: Recipe = serde_json::from_slice(&bytes).unwrap();
    validate_recipe(&restored).unwrap();
    assert_eq!(restored.color_grading.shadows.hue, 240.0);
    assert_eq!(
        restored.masks[0].adjustments.color_grading.highlights.hue,
        300.0
    );
}

#[test]
fn grading_preserves_legacy_values_but_rejects_invalid_values_and_hsl_overflow() {
    let mut recipe = Recipe::default();
    recipe.color_grading.global.hue = -100.0;
    recipe.color_grading.global.saturation = -50.0;
    validate_recipe(&recipe).unwrap();
    for invalid in [-101.0, 361.0, f64::NAN, f64::INFINITY] {
        recipe.color_grading.global.hue = invalid;
        assert!(validate_recipe(&recipe).is_err(), "accepted hue {invalid}");
    }
    recipe.color_grading.global.hue = 240.0;
    recipe.hsl.blues.hue = 101.0;
    assert!(
        validate_recipe(&recipe)
            .unwrap_err()
            .to_string()
            .contains("hsl.blues.hue")
    );
}
