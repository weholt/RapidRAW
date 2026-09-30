use rapidraw_develop::gpu::params::get_all_adjustments_from_json;

#[test]
fn extracted_recipe_controls_reach_the_shared_gpu_uniforms() {
    let adjustments = serde_json::json!({
        "levels": {"enabled": true, "rgb": {"inputBlack": 16, "inputWhite": 235}},
        "blackWhiteEnabled": true,
        "blackWhiteMix": [35, 0, 0, 0, 0, 0, 0, -20],
        "vignetting": {"enabled": true, "amount": -1.5, "method": "circularOnCrop"}
    });
    let gpu = get_all_adjustments_from_json(&adjustments, true, None);

    assert_eq!(gpu.global.black_white_enabled, 1);
    assert_eq!(gpu.global.black_white_mix0[0], 0.35);
    assert_eq!(gpu.global.black_white_mix1[3], -0.2);
    assert_eq!(gpu.global.vignetting[0], -1.5);
    assert_eq!(gpu.global.vignetting[1], 1.0);
    assert_eq!(gpu.global.levels[0].active, 1);
    assert!((gpu.global.levels[0].input_black - 16.0 / 255.0).abs() < 0.0001);
}
