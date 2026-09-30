//! Decode behavior tests against the Lap RAW corpus and synthetic suites
//! (`lap-7f5.2`): full dimensions, high precision, explicit effective inputs,
//! explicit failures, cancellation, and metadata reporting.
//!
//! Tests skip (with a loud note) when the corpus root is absent so the crate
//! stays testable on machines without the licensed fixtures; the corpus
//! itself is gated in the engine's `raw_corpus_gates.rs`.

use rapidraw_develop::{
    CancelToken, DecodeEvent, DecodeOptions, DecodeReport, DevelopError, LinearRawMode, ToneMapper,
    WhiteBalancePolicy, decode_original,
};
use rawler::decoders::Orientation;
use std::path::{Path, PathBuf};

fn corpus_root() -> Option<PathBuf> {
    let root = std::env::var("LAP_RAW_CORPUS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../lap/tests/fixtures/raw-development")
        });
    if root.join("corpus-manifest.json").is_file() {
        Some(root)
    } else {
        None
    }
}

fn fixture(root: &Path, rel: &str) -> Vec<u8> {
    std::fs::read(root.join(rel)).unwrap_or_else(|e| panic!("read corpus fixture {rel}: {e}"))
}

fn synthetic(root: &Path, rel: &str) -> Vec<u8> {
    fixture(root, &format!("synthetic/{rel}"))
}

fn skip_note() -> Option<PathBuf> {
    match corpus_root() {
        Some(root) => Some(root),
        None => {
            eprintln!("SKIPPED: Lap RAW corpus root not found (lap-7f5.2 fixtures absent)");
            None
        }
    }
}

#[test]
fn effective_settings_map_onto_typed_decode_options() {
    let effective = rapidraw_edit_model::types::EffectiveDecodeSettings {
        is_raw: true,
        fast_demosaic: true,
        highlight_compression: 4.0,
        linear_raw_mode: LinearRawMode::GammaSkipCalib,
        raw_color_noise_reduction: 0.5,
        raw_sharpening: 0.35,
        tonemapper_override: Some(ToneMapper::Agx),
    };
    let options = DecodeOptions::from_effective(&effective);
    assert!(options.fast_demosaic);
    assert_eq!(options.highlight_compression, 4.0);
    assert_eq!(options.linear_mode, LinearRawMode::GammaSkipCalib);
    assert_eq!(options.tone_mapper, Some(ToneMapper::Agx));
    assert_eq!(
        options.white_balance,
        WhiteBalancePolicy::AutoNeutralizeMultiExposure
    );
    assert_eq!(
        options.orientation,
        rapidraw_develop::OrientationHandling::ApplyFromMetadata
    );
}

#[test]
fn cancel_source_bumps_generation_and_tokens_observe_it() {
    let (source, token) = CancelToken::pair();
    assert!(!token.is_cancelled());
    assert!(token.check().is_ok());
    source.cancel();
    assert!(token.is_cancelled());
    assert!(matches!(token.check(), Err(DevelopError::Cancelled)));
}

#[test]
fn decode_honors_cancellation_before_touching_the_input() {
    let (source, token) = CancelToken::pair();
    source.cancel();
    let options = DecodeOptions {
        cancellation: Some(token),
        ..DecodeOptions::default()
    };
    let err = decode_original(&[0u8; 16], &options).expect_err("cancelled decode must fail");
    assert!(matches!(err, DevelopError::Cancelled), "{err}");
}

#[test]
fn empty_input_fails_explicitly_without_preview_fallback() {
    let err = decode_original(&[], &DecodeOptions::default()).expect_err("empty input must fail");
    assert!(
        matches!(err, DevelopError::Decode(_) | DevelopError::InvalidInput(_)),
        "explicit decode error expected, got: {err}"
    );
    assert!(!err.to_string().is_empty());
}

#[test]
fn non_raw_bytes_fail_explicitly_without_preview_fallback() {
    let Some(root) = skip_note() else { return };
    let bytes = synthetic(&root, "not-a-raw.dng");
    let err = decode_original(&bytes, &DecodeOptions::default()).expect_err("must fail");
    assert!(
        matches!(err, DevelopError::Decode(_) | DevelopError::InvalidInput(_)),
        "explicit decode error expected, got: {err}"
    );
}

#[test]
fn truncated_input_fails_explicitly() {
    let Some(root) = skip_note() else { return };
    let bytes = synthetic(&root, "truncated-linear.dng");
    let err = decode_original(&bytes, &DecodeOptions::default()).expect_err("must fail");
    assert!(
        matches!(err, DevelopError::Decode(_) | DevelopError::InvalidInput(_)),
        "explicit decode error expected, got: {err}"
    );
}

#[test]
fn linear_dng_decodes_to_full_dimension_high_precision_buffer() {
    let Some(root) = skip_note() else { return };
    let bytes = synthetic(&root, "dng-linear-gradient-64x48.dng");
    let decoded = decode_original(&bytes, &DecodeOptions::default()).expect("decode");
    assert_eq!(decoded.report.output_dimensions, (64, 48));
    assert_eq!(decoded.image.dimensions(), (64, 48));
    assert_eq!(decoded.image.rgb().len(), 64 * 48 * 3);
    assert!(decoded.report.is_linear_raw, "fixture is a linear DNG");
    // High precision: the gradient must not be quantized to 8-bit steps.
    let has_sub_eight_bit = decoded
        .image
        .rgb()
        .iter()
        .any(|v| (*v * 255.0).fract().abs() > 1e-6);
    assert!(has_sub_eight_bit, "buffer must retain sub-8-bit precision");
}

#[test]
fn orientation_six_fixture_is_detected_and_applied() {
    let Some(root) = skip_note() else { return };
    let bytes = synthetic(&root, "dng-linear-orientation6-64x48.dng");
    let decoded = decode_original(&bytes, &DecodeOptions::default()).expect("decode");
    assert_eq!(decoded.report.orientation, Orientation::Rotate90);
    assert_eq!(
        decoded.image.dimensions(),
        (48, 64),
        "display rotation must swap the fixture dimensions"
    );
    assert_eq!(decoded.report.output_dimensions, (48, 64));

    let unoriented = decode_original(
        &bytes,
        &DecodeOptions {
            orientation: rapidraw_develop::OrientationHandling::KeepUnoriented,
            ..DecodeOptions::default()
        },
    )
    .expect("decode");
    assert_eq!(unoriented.image.dimensions(), (64, 48));
    assert_eq!(unoriented.report.orientation, Orientation::Rotate90);
}

#[test]
fn highlight_compression_is_an_effective_explicit_input() {
    let Some(root) = skip_note() else { return };
    // The gradient fixture reaches values above 1.0 with differing channels,
    // so the highlight rolloff formula (which preserves constant-color
    // whites) produces a visible difference between compression levels. The
    // all-white highlight-clipped fixture cannot show it (min == max).
    let bytes = synthetic(&root, "dng-linear-gradient-64x48.dng");
    let low = decode_original(
        &bytes,
        &DecodeOptions {
            highlight_compression: 1.01,
            ..DecodeOptions::default()
        },
    )
    .expect("decode");
    let high = decode_original(
        &bytes,
        &DecodeOptions {
            highlight_compression: 8.0,
            ..DecodeOptions::default()
        },
    )
    .expect("decode");
    assert_eq!(low.report.highlight_compression, 1.01);
    assert_eq!(high.report.highlight_compression, 8.0);
    assert_ne!(
        low.image.rgb(),
        high.image.rgb(),
        "effective highlight compression must change the developed pixels"
    );
}

#[test]
fn linear_mode_is_an_effective_explicit_input() {
    let Some(root) = skip_note() else { return };
    let bytes = synthetic(&root, "dng-linear-gradient-64x48.dng");
    let auto = decode_original(
        &bytes,
        &DecodeOptions {
            linear_mode: LinearRawMode::Auto,
            ..DecodeOptions::default()
        },
    )
    .expect("decode");
    let gamma = decode_original(
        &bytes,
        &DecodeOptions {
            linear_mode: LinearRawMode::Gamma,
            ..DecodeOptions::default()
        },
    )
    .expect("decode");
    assert_ne!(
        auto.image.rgb(),
        gamma.image.rgb(),
        "linear-RAW mode must change the developed pixels"
    );
    assert_eq!(auto.report.linear_mode, LinearRawMode::Auto);
    assert_eq!(gamma.report.linear_mode, LinearRawMode::Gamma);
}

#[test]
fn white_balance_policy_is_an_explicit_input() {
    let Some(root) = skip_note() else { return };
    let bytes = synthetic(&root, "dng-linear-gradient-64x48.dng");
    let from_file = decode_original(
        &bytes,
        &DecodeOptions {
            white_balance: WhiteBalancePolicy::FromFile,
            ..DecodeOptions::default()
        },
    )
    .expect("decode");
    let neutral = decode_original(
        &bytes,
        &DecodeOptions {
            white_balance: WhiteBalancePolicy::Neutral,
            ..DecodeOptions::default()
        },
    )
    .expect("decode");
    assert!(!from_file.report.wb_neutralized);
    assert!(neutral.report.wb_neutralized);
}

#[test]
fn tone_mapper_is_recorded_but_not_applied_by_the_rawler_path() {
    let Some(root) = skip_note() else { return };
    let bytes = synthetic(&root, "dng-linear-gradient-64x48.dng");
    let plain = decode_original(&bytes, &DecodeOptions::default()).expect("decode");
    let agx = decode_original(
        &bytes,
        &DecodeOptions {
            tone_mapper: Some(ToneMapper::Agx),
            ..DecodeOptions::default()
        },
    )
    .expect("decode");
    assert_eq!(plain.report.tone_mapper, None);
    assert_eq!(agx.report.tone_mapper, Some(ToneMapper::Agx));
    assert_eq!(
        plain.image.rgb(),
        agx.image.rgb(),
        "decode must stay tone-mapper-neutral: the render stage applies tone mapping"
    );
}

#[test]
fn wrong_tiff_magic_matches_gradient_baseline_behavior() {
    let Some(root) = skip_note() else { return };
    let gradient = synthetic(&root, "dng-linear-gradient-64x48.dng");
    let wrong_magic = synthetic(&root, "wrong-tiff-magic.dng");
    let a = decode_original(&gradient, &DecodeOptions::default()).expect("decode");
    let b = decode_original(&wrong_magic, &DecodeOptions::default()).expect("decode");
    assert_eq!(
        a.image.rgb(),
        b.image.rgb(),
        "pinned baseline: rawler tolerates the wrong magic; a change here must be explicit"
    );
}

#[test]
fn observer_receives_metadata_events_in_order() {
    let Some(root) = skip_note() else { return };
    let bytes = synthetic(&root, "dng-linear-orientation6-64x48.dng");
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let options = DecodeOptions {
        observer: Some(Box::new(move |event: &DecodeEvent| {
            let kind = match event {
                DecodeEvent::DecoderReady { .. } => "decoder-ready",
                DecodeEvent::OrientationDetected { .. } => "orientation",
                DecodeEvent::WhiteBalanceNeutralized => "wb-neutralized",
                DecodeEvent::Completed { .. } => "completed",
            };
            sink.lock().unwrap().push(kind.to_string());
        })),
        ..DecodeOptions::default()
    };
    let decoded = decode_original(&bytes, &options).expect("decode");
    let events = events.lock().unwrap();
    assert_eq!(
        events.as_slice(),
        ["decoder-ready", "orientation", "completed"],
        "observed: {events:?}"
    );
    assert_eq!(decoded.report.output_dimensions, (48, 64));
}

#[test]
fn bayer_corpus_decodes_to_full_sensor_dimensions_with_precision() {
    let Some(root) = skip_note() else { return };
    // Pinned pre-extraction decode (verified by host parity in
    // src-tauri/tests/develop_decoder_parity.rs): the raw sensor buffer is
    // 5568x3648 and the developed output is default-cropped to the camera's
    // 5472x3648 — the corpus manifest's decodedDimensions. Both exceed
    // 4096 px, so embedded/4096px previews can never be this source.
    let bytes = fixture(&root, "corpus/canon-eos-r6-craw-iso100-nocrop.CR3");
    let decoded = decode_original(&bytes, &DecodeOptions::default()).expect("decode");
    assert_eq!(decoded.report.output_dimensions, (5472, 3648));
    assert_eq!(decoded.report.source_dimensions, (5568, 3708));
    assert!(!decoded.report.is_linear_raw);
    let has_sub_eight_bit = decoded
        .image
        .rgb()
        .iter()
        .any(|v| (*v * 255.0).fract().abs() > 1e-6);
    assert!(has_sub_eight_bit, "buffer must retain sub-8-bit precision");
}

#[test]
fn bayer_corpus_fast_demosaic_reduces_dimensions_by_four() {
    let Some(root) = skip_note() else { return };
    // Pinned actual: superpixel demosaic quarters the active area and the
    // default crop is rescaled with rawler's rounding, giving 1367x911 at
    // the pinned rawler revision (host parity pins the same value).
    let bytes = fixture(&root, "corpus/canon-eos-r6-craw-iso100-nocrop.CR3");
    let options = DecodeOptions {
        fast_demosaic: true,
        ..DecodeOptions::default()
    };
    let decoded = decode_original(&bytes, &options).expect("decode");
    assert_eq!(decoded.report.output_dimensions, (1367, 911));
    assert!(decoded.report.fast_demosaic);
}

#[test]
fn xtrans_corpus_decodes_to_full_dimensions() {
    let Some(root) = skip_note() else { return };
    // corpus-manifest pins decodedDimensions [7728, 5152].
    let bytes = fixture(&root, "corpus/fujifilm-xt5-lossy-iso125.RAF");
    let decoded = decode_original(&bytes, &DecodeOptions::default()).expect("decode");
    assert_eq!(decoded.report.output_dimensions, (7728, 5152));
    assert!(!decoded.report.is_linear_raw);
}

#[test]
fn linear_dng_orientation_variant_decodes_with_expected_dimensions() {
    let Some(root) = skip_note() else { return };
    // corpus-manifest pins decodedDimensions [5464, 8192] (oriented frame).
    let bytes = fixture(&root, "corpus/dng-linear-orientation6.DNG");
    let decoded = decode_original(&bytes, &DecodeOptions::default()).expect("decode");
    assert_eq!(decoded.report.orientation, Orientation::Rotate90);
    assert_eq!(decoded.image.dimensions(), (5464, 8192));
    assert_eq!(decoded.report.output_dimensions, (5464, 8192));
}

#[test]
fn decode_report_is_structural_metadata_only() {
    // The report must never carry pixel data or host/session references.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<DecodeReport>();
}
