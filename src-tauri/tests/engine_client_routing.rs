//! Engine-client routing tests (lap-517 / TASK-205, engine issue
//! rapidraw-d73): the host's RAW decode entry points must produce their
//! pixels through the extracted `rapidraw-develop` engine crate via a typed
//! adapter, surface the engine decode metadata, and preserve the public host
//! semantics: identical pixels, identical linear-mode string mapping
//! ("auto"/"gamma"/"skip_calib"/"gamma_skip_calib"), and the historic
//! "Load cancelled" error text that the loader classifies on.
//!
//! Tests skip (with a loud note) when the Lap RAW corpus root is absent; the
//! corpus itself is gated by `raw_corpus_gates.rs`.

use rapidraw_develop::{DecodeOptions, LinearRawMode, decode_original};
use std::path::{Path, PathBuf};

fn corpus_root() -> Option<PathBuf> {
    let root = std::env::var("LAP_RAW_CORPUS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lap/tests/fixtures/raw-development")
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

fn host_pixels(img: &image::DynamicImage) -> Vec<f32> {
    img.to_rgba32f().into_raw()
}

#[test]
fn reported_decode_routes_through_engine_and_matches_crate_pixels() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "corpus/canon-eos-r6-craw-iso100-nocrop.CR3");
    let (host, report) = rapidraw_lib::raw_processing::develop_raw_image_reported(
        &bytes,
        false,
        2.5,
        "auto".to_string(),
        None,
    )
    .expect("host reported decode");
    let engine = decode_original(&bytes, &DecodeOptions::default()).expect("engine decode");

    assert_eq!(
        host_pixels(&host),
        engine.image.rgba32f(),
        "host pixels must come from the engine decode"
    );
    assert_eq!(report, engine.report, "host must surface the engine report");
    assert!(!report.wb_neutralized);
    assert!(!report.is_linear_raw);
}

#[test]
fn host_reports_engine_decode_metadata_for_linear_orientation() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "synthetic/dng-linear-orientation6-64x48.dng");
    let (host, report) = rapidraw_lib::raw_processing::develop_raw_image_reported(
        &bytes,
        false,
        2.5,
        "auto".to_string(),
        None,
    )
    .expect("host reported decode");

    assert!(
        report.is_linear_raw,
        "synthetic linear DNG must be reported"
    );
    assert_eq!(report.source_dimensions, (64, 48));
    assert_eq!(report.output_dimensions, (48, 64));
    assert_eq!((host.width(), host.height()), (48, 64));
    let engine = decode_original(&bytes, &DecodeOptions::default()).expect("engine decode");
    assert_eq!(host_pixels(&host), engine.image.rgba32f());
}

#[test]
fn adapter_maps_cancellation_to_load_cancelled() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "synthetic/dng-linear-gradient-64x48.dng");
    let tracker = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(5));
    // The loader bumps the shared tracker whenever a newer load supersedes
    // the outstanding one; the stale (tracker, 5) pair must then fail fast.
    tracker.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let err = rapidraw_lib::raw_processing::develop_raw_image_reported(
        &bytes,
        false,
        2.5,
        "auto".to_string(),
        Some((tracker.clone(), 5)),
    )
    .expect_err("a cancelled decode must fail");
    assert!(
        err.to_string().contains("Load cancelled"),
        "historic loader classification text must be preserved, got: {err}"
    );
}

#[test]
fn adapter_maps_linear_mode_strings_like_the_host() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "synthetic/dng-linear-gradient-64x48.dng");
    for (mode, expected) in [
        ("auto", LinearRawMode::Auto),
        ("gamma", LinearRawMode::Gamma),
        ("skip_calib", LinearRawMode::SkipCalib),
        ("gamma_skip_calib", LinearRawMode::GammaSkipCalib),
    ] {
        let (host, report) = rapidraw_lib::raw_processing::develop_raw_image_reported(
            &bytes,
            false,
            2.5,
            mode.to_string(),
            None,
        )
        .unwrap_or_else(|e| panic!("host decode for {mode}: {e}"));
        let engine = decode_original(
            &bytes,
            &DecodeOptions {
                linear_mode: expected,
                ..DecodeOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("engine decode for {mode}: {e}"));
        assert_eq!(report.linear_mode, expected, "mode {mode}");
        assert_eq!(
            host_pixels(&host),
            engine.image.rgba32f(),
            "mode {mode}: pixels must match the engine decode"
        );
    }
}

#[test]
fn fast_demosaic_public_path_still_matches_engine() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "corpus/canon-eos-r6-craw-iso100-nocrop.CR3");
    let host = rapidraw_lib::raw_processing::develop_raw_image(
        &bytes,
        true,
        2.5,
        "auto".to_string(),
        None,
    )
    .expect("host fast decode");
    let engine = decode_original(
        &bytes,
        &DecodeOptions {
            fast_demosaic: true,
            ..DecodeOptions::default()
        },
    )
    .expect("engine fast decode");
    // Pinned pre-extraction behavior: superpixel demosaic plus the rescaled
    // default crop yield 1367x911.
    assert_eq!(engine.image.dimensions(), (1367, 911));
    assert_eq!(host_pixels(&host), engine.image.rgba32f());
}

#[test]
fn develop_raw_image_output_equals_reported_output() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "synthetic/dng-linear-gradient-64x48.dng");
    let plain = rapidraw_lib::raw_processing::develop_raw_image(
        &bytes,
        false,
        2.5,
        "auto".to_string(),
        None,
    )
    .expect("host decode");
    let (reported, _) = rapidraw_lib::raw_processing::develop_raw_image_reported(
        &bytes,
        false,
        2.5,
        "auto".to_string(),
        None,
    )
    .expect("host reported decode");
    assert_eq!(host_pixels(&plain), host_pixels(&reported));
}
