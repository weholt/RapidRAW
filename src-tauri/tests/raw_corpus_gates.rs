//! Corpus gates for the Lap RAW-development regression corpus
//! (`lap-7f5.2` / TASK-102, engine issue `rapidraw-39c`).
//!
//! These tests protect prerequisite P4 of `docs/raw-development/spec.md` in the
//! Lap repository: before any extraction change touches the engine, a licensed
//! RAW corpus and a pinned pre-extraction baseline manifest must exist and stay
//! intact. The gates deliberately run without a GPU or a Tauri app: they verify
//! fixture bytes, provenance, and manifest structure only. The actual baseline
//! capture is driven from the Lap checkout against this pinned engine revision.
//!
//! The corpus lives in the sibling Lap checkout. Override the location with
//! `LAP_RAW_CORPUS_ROOT`; the default resolves `../../lap` relative to this
//! crate, matching the dedicated two-checkout layout
//! (`C:/Users/Thomas/Desktop/lap`, `C:/Users/Thomas/Desktop/RapidRAW-engine`).

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const CORPUS_SCHEMA: &str = "lap-raw-corpus/v1";
const BASELINE_SCHEMA: &str = "lap-raw-baseline/v1";
const PINNED_RENDER_BASELINE: &str = "5e30bcbb246395d391ba2e9662510641ffe68e6b";
/// Engine sources that participate in rendering a RAW to an exported image.
/// The baseline manifest pins a content hash for each; any change here means
/// the baseline was captured against a different renderer and must be
/// re-examined, never silently reused.
const RENDER_RELEVANT_SOURCES: &[&str] = &[
    "src-tauri/src/raw_processing.rs",
    "src-tauri/src/image_processing.rs",
    "src-tauri/src/gpu_processing.rs",
    "src-tauri/src/export_processing.rs",
    "src-tauri/src/adjustment_utils.rs",
    "src-tauri/src/image_loader.rs",
];

fn corpus_root() -> PathBuf {
    if let Ok(root) = std::env::var("LAP_RAW_CORPUS_ROOT") {
        return PathBuf::from(root);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lap/tests/fixtures/raw-development")
}

fn read_json(path: &Path) -> serde_json::Value {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn sha256_file(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hex::encode(hasher.finalize())
}

fn entries(manifest: &serde_json::Value) -> Vec<&serde_json::Value> {
    manifest["files"]
        .as_array()
        .unwrap_or_else(|| panic!("corpus manifest is missing the files array"))
        .iter()
        .collect()
}

#[test]
fn corpus_manifest_exists_with_required_schema() {
    let manifest_path = corpus_root().join("corpus-manifest.json");
    assert!(
        manifest_path.is_file(),
        "corpus manifest missing at {}. Run scripts/raw-development/acquire-corpus.ps1 \
         in the Lap repository to build the corpus (lap-7f5.2).",
        manifest_path.display()
    );
    let manifest = read_json(&manifest_path);
    assert_eq!(
        manifest["schema"].as_str().unwrap_or_default(),
        CORPUS_SCHEMA,
        "unexpected corpus manifest schema"
    );
    assert!(!entries(&manifest).is_empty(), "corpus must not be empty");
}

#[test]
fn every_corpus_file_exists_and_matches_its_checksum() {
    let manifest = read_json(&corpus_root().join("corpus-manifest.json"));
    for entry in entries(&manifest) {
        let rel = entry["path"].as_str().unwrap_or_else(|| {
            panic!("corpus entry without a path: {entry}");
        });
        let file = corpus_root().join(rel);
        assert!(file.is_file(), "corpus file missing: {}", file.display());
        let expected = entry["sha256"].as_str().unwrap_or_default().to_lowercase();
        assert_eq!(
            expected.len(),
            64,
            "corpus entry {rel} lacks a sha256 checksum"
        );
        let actual = sha256_file(&file);
        assert_eq!(
            actual, expected,
            "checksum mismatch for {rel}: corpus file was modified after acquisition"
        );
    }
}

#[test]
fn every_corpus_entry_records_provenance_and_permitted_use() {
    let manifest = read_json(&corpus_root().join("corpus-manifest.json"));
    for entry in entries(&manifest) {
        let rel = entry["path"].as_str().unwrap_or("<missing path>");
        let origin = entry["origin"].as_str().unwrap_or_default();
        assert!(
            origin == "rawdb.dnglab.org" || origin == "derived" || origin == "synthetic",
            "corpus entry {rel} has unknown origin {origin:?}"
        );
        if origin == "rawdb.dnglab.org" {
            assert!(
                entry["sourceUrl"]
                    .as_str()
                    .is_some_and(|u| u.starts_with("https://rawdb.dnglab.org/")),
                "corpus entry {rel} from rawdb must record its source URL"
            );
        }
        if origin == "derived" {
            assert!(
                entry["derivedFrom"].is_object() || entry["derivedFrom"].is_string(),
                "derived corpus entry {rel} must record its derivation input"
            );
        }
        let license = entry["license"].as_str().unwrap_or_default();
        assert_eq!(
            license, "CC0-1.0",
            "corpus entry {rel} must carry CC0-1.0 permitted-use evidence, got {license:?}"
        );
    }
}

#[test]
fn corpus_covers_required_raw_categories() {
    let manifest = read_json(&corpus_root().join("corpus-manifest.json"));
    let mut categories: Vec<String> = entries(&manifest)
        .iter()
        .filter_map(|e| e["categories"].as_array())
        .flatten()
        .filter_map(|c| c.as_str().map(String::from))
        .collect();
    for required in [
        "bayer",
        "xtrans",
        "linear-dng",
        "orientation",
        "highlight-stress",
    ] {
        assert!(
            categories.contains(&required.to_string()),
            "corpus is missing required category {required}"
        );
    }
    categories.dedup();

    let large = entries(&manifest).iter().any(|e| {
        e["decodedDimensions"]
            .as_array()
            .is_some_and(|dims| dims.iter().filter_map(|d| d.as_u64()).any(|d| d > 4096))
            || e["maxDimension"].as_u64().is_some_and(|d| d > 4096)
    });
    assert!(
        large,
        "corpus must contain a RAW whose decoded size exceeds 4096 pixels"
    );
}

#[test]
fn baseline_manifest_pins_engine_revision_decode_options_and_outputs() {
    let manifest_path = corpus_root().join("baselines/capture-manifest.json");
    assert!(
        manifest_path.is_file(),
        "baseline manifest missing at {}. Capture it with \
         scripts/raw-development/capture-baselines.ps1 in the Lap repository (lap-7f5.2).",
        manifest_path.display()
    );
    let manifest = read_json(&manifest_path);
    assert_eq!(
        manifest["schema"].as_str().unwrap_or_default(),
        BASELINE_SCHEMA,
        "unexpected baseline manifest schema"
    );

    let engine = &manifest["engine"];
    assert!(
        engine["commit"].as_str().is_some_and(|c| c.len() >= 7),
        "baseline must record the engine commit it was captured against"
    );
    assert!(
        engine["renderBaselineCommit"]
            .as_str()
            .is_some_and(|c| c == PINNED_RENDER_BASELINE),
        "baseline must pin the pre-extraction render revision {PINNED_RENDER_BASELINE}"
    );
    let hashes = engine["renderSourceSha256"].as_object().unwrap_or_else(|| {
        panic!("baseline must pin sha256 hashes for render-relevant engine sources")
    });
    for source in RENDER_RELEVANT_SOURCES {
        assert!(
            hashes
                .get(*source)
                .is_some_and(|v| v.as_str().is_some_and(|h| h.len() == 64)),
            "baseline engine pin is missing source {source}"
        );
    }

    assert!(
        engine["gpu"]["backend"]
            .as_str()
            .is_some_and(|b| !b.is_empty()),
        "baseline must record the GPU backend used for capture"
    );
    assert!(
        engine["gpu"]["adapter"]
            .as_str()
            .is_some_and(|a| !a.is_empty()),
        "baseline must record the GPU adapter used for capture"
    );

    for key in ["decodeOptions", "colorSpaces", "defaults"] {
        assert!(
            manifest[key].is_object(),
            "baseline manifest must record {key}"
        );
    }
    let decode = &manifest["decodeOptions"];
    for key in [
        "highlightCompression",
        "linearRawMode",
        "preprocessingColorNoiseReduction",
        "preprocessingSharpening",
        "fastDemosaic",
    ] {
        assert!(!decode[key].is_null(), "decodeOptions missing {key}");
    }

    let cases = manifest["cases"]
        .as_array()
        .unwrap_or_else(|| panic!("baseline manifest must record capture cases"));
    assert!(!cases.is_empty(), "baseline has no captured cases");
    let has_default = cases
        .iter()
        .any(|c| c["preset"].as_str().is_some_and(|p| p == "default"));
    assert!(has_default, "baseline must include the default render case");
    for case in cases {
        let preset = case["preset"].as_str().unwrap_or("<missing>");
        assert!(
            case["fixture"].as_str().is_some(),
            "baseline case {preset} does not name its fixture"
        );
        // Failure-expectation cases pin explicit rejection (nonzero exit +
        // logged error evidence), never a render checksum.
        if case["expectFailure"].as_bool().unwrap_or(false) {
            assert!(
                case["exitCode"].as_i64().is_some_and(|c| c != 0),
                "failure-expectation case {preset} must record a nonzero exit code"
            );
            assert!(
                case["errorEvidence"]
                    .as_str()
                    .is_some_and(|e| !e.is_empty()),
                "failure-expectation case {preset} must record logged error evidence"
            );
            continue;
        }
        assert!(
            case["outputSha256"].as_str().is_some_and(|h| h.len() == 64),
            "baseline case {preset} lacks an output checksum"
        );
        assert!(
            case["outputDimensions"]
                .as_array()
                .is_some_and(|d| d.len() == 2),
            "baseline case {preset} lacks output dimensions"
        );
    }
    let per_adjustment = cases
        .iter()
        .filter(|c| c["kind"].as_str().is_some_and(|k| k == "per-adjustment"))
        .count();
    assert!(
        per_adjustment >= 5,
        "baseline must include per-adjustment cases for representative controls"
    );
    let combined = cases
        .iter()
        .filter(|c| c["kind"].as_str().is_some_and(|k| k == "combined"))
        .count();
    assert!(combined >= 1, "baseline must include a combined case");

    assert!(
        manifest["determinism"].is_object(),
        "baseline must characterize run-to-run determinism"
    );
}

#[test]
fn render_relevant_engine_sources_still_match_the_pinned_baseline() {
    let manifest_path = corpus_root().join("baselines/capture-manifest.json");
    assert!(
        manifest_path.is_file(),
        "baseline manifest missing; capture it before comparing engine sources"
    );
    let manifest = read_json(&manifest_path);
    let hashes = manifest["engine"]["renderSourceSha256"]
        .as_object()
        .expect("pinned render source hashes");
    // CARGO_MANIFEST_DIR is src-tauri, so the engine root is one level up
    // (unlike corpus_root, which crosses ../.. to the sibling Lap checkout).
    let engine_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("resolve engine repository root");
    for (source, pinned) in hashes {
        let pinned = pinned.as_str().unwrap_or_default().to_lowercase();
        let file = engine_root.join(source);
        assert!(
            file.is_file(),
            "pinned engine source {source} no longer exists"
        );
        let actual = sha256_file(&file);
        assert_eq!(
            actual, pinned,
            "engine source {source} changed since the baseline was captured; \
             the stored baseline belongs to the pre-extraction renderer and must \
             be re-captured deliberately (never auto-regenerated)"
        );
    }
}
