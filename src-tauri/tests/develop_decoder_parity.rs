//! Host-vs-crate decoder parity: the extracted `rapidraw-develop` decoder
//! must reproduce the pre-extraction host pipeline
//! (`rapidraw_lib::raw_processing::develop_raw_image`) exactly on the Lap RAW
//! corpus (lap-7f5.2 fixtures) before any host call is replaced. Geometry
//! operations are additionally checked against the `image` crate primitives
//! that the host's `apply_orientation` composes.
//!
//! Tests skip (with a loud note) when the corpus root is absent; the corpus
//! itself is gated by `raw_corpus_gates.rs`.

use rapidraw_develop::{DecodeOptions, decode_original};
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

fn host_decode(bytes: &[u8]) -> image::DynamicImage {
    rapidraw_lib::raw_processing::develop_raw_image(bytes, false, 2.5, "auto".to_string(), None)
        .expect("host decode")
}

fn crate_decode(bytes: &[u8]) -> rapidraw_develop::DecodedOriginal {
    decode_original(bytes, &DecodeOptions::default()).expect("crate decode")
}

/// Assert the crate decode reproduces the host decode exactly: same
/// dimensions, same linear f32 samples (including the opaque alpha channel),
/// same orientation.
fn assert_decode_parity(bytes: &[u8], context: &str) {
    let host = host_decode(bytes);
    let extracted = crate_decode(bytes);
    let host_rgba = host.to_rgba32f().into_raw();
    let crate_rgba = extracted.image.rgba32f();

    assert_eq!(
        (host.width(), host.height()),
        extracted.image.dimensions(),
        "{context}: dimensions diverge"
    );
    assert_eq!(
        host_rgba.len(),
        crate_rgba.len(),
        "{context}: sample count diverges"
    );
    for (i, (h, c)) in host_rgba.iter().zip(crate_rgba.iter()).enumerate() {
        assert_eq!(
            h,
            c,
            "{context}: sample {i} (pixel {}, channel {}) diverges: host {h} vs crate {c}",
            i / 4,
            i % 4
        );
    }
}

#[test]
fn parity_synthetic_linear_gradient() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "synthetic/dng-linear-gradient-64x48.dng");
    assert_decode_parity(&bytes, "linear-gradient-64x48");
}

#[test]
fn parity_synthetic_orientation6() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "synthetic/dng-linear-orientation6-64x48.dng");
    assert_decode_parity(&bytes, "linear-orientation6-64x48");
}

#[test]
fn parity_synthetic_highlight_clipped() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "synthetic/dng-highlight-clipped-64x48.dng");
    assert_decode_parity(&bytes, "highlight-clipped-64x48");
}

#[test]
fn parity_synthetic_wrong_tiff_magic() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "synthetic/wrong-tiff-magic.dng");
    assert_decode_parity(&bytes, "wrong-tiff-magic");
}

#[test]
fn parity_synthetic_extreme_constants() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "synthetic/dng-extreme-constants-8x2.dng");
    assert_decode_parity(&bytes, "extreme-constants-8x2");
}

#[test]
fn parity_synthetic_wide_5000() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "synthetic/dng-linear-wide-5000x64.dng");
    assert_decode_parity(&bytes, "linear-wide-5000x64");
}

#[test]
fn parity_bayer_corpus_cr3() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "corpus/canon-eos-r6-craw-iso100-nocrop.CR3");
    assert_decode_parity(&bytes, "canon-eos-r6-craw-iso100");
}

#[test]
fn parity_xtrans_corpus_raf() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "corpus/fujifilm-xt5-lossy-iso125.RAF");
    assert_decode_parity(&bytes, "fujifilm-xt5-lossy");
}

#[test]
fn parity_linear_dng_jpegxl() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "corpus/dng-jpegxl-lossy-16bit-linear-tiles.DNG");
    assert_decode_parity(&bytes, "dng-jpegxl-linear-tiles");
}

#[test]
fn parity_linear_dng_orientation6() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "corpus/dng-linear-orientation6.DNG");
    assert_decode_parity(&bytes, "dng-linear-orientation6");
}

#[test]
fn parity_high_iso_bayer_corpus_cr3() {
    let Some(root) = corpus_root() else {
        eprintln!("SKIPPED: corpus root absent");
        return;
    };
    let bytes = fixture(&root, "corpus/canon-eos-r6-craw-iso204800-nocrop.CR3");
    assert_decode_parity(&bytes, "canon-eos-r6-craw-iso204800");
}

// ---------------------------------------------------------------------------
// Geometry parity against the `image` crate primitives the host composes in
// `apply_orientation` (Normal=identity, HorizontalFlip=fliph, Rotate180,
// VerticalFlip=flipv, Transpose=rotate90().fliph(), Rotate90=rotate90,
// Transverse=rotate270().fliph(), Rotate270=rotate270).
// ---------------------------------------------------------------------------

fn host_like_orientation(
    image: &image::DynamicImage,
    orientation: rawler::decoders::Orientation,
) -> image::DynamicImage {
    use rawler::decoders::Orientation;
    match orientation {
        Orientation::Normal | Orientation::Unknown => image.clone(),
        Orientation::HorizontalFlip => image.fliph(),
        Orientation::Rotate180 => image.rotate180(),
        Orientation::VerticalFlip => image.flipv(),
        Orientation::Transpose => image.rotate90().fliph(),
        Orientation::Rotate90 => image.rotate90(),
        Orientation::Transverse => image.rotate270().fliph(),
        Orientation::Rotate270 => image.rotate270(),
    }
}

#[test]
fn geometry_orientation_matches_host_image_crate_semantics() {
    use rawler::decoders::Orientation;

    // Non-square so quarter turns are distinguishable; distinct pixels.
    let host_image =
        image::DynamicImage::ImageRgba32F(image::ImageBuffer::from_fn(5, 3, |x, y| {
            let v = (y * 5 + x) as f32 * 0.137 + 0.01;
            image::Rgba([v, v * 2.0, v * 3.0, 1.0])
        }));
    let crate_image = rapidraw_develop::LinearImage::from_fn(5, 3, |x, y| {
        let v = (y * 5 + x) as f32 * 0.137 + 0.01;
        [v, v * 2.0, v * 3.0]
    });

    for orientation in [
        Orientation::Normal,
        Orientation::Unknown,
        Orientation::HorizontalFlip,
        Orientation::Rotate180,
        Orientation::VerticalFlip,
        Orientation::Transpose,
        Orientation::Rotate90,
        Orientation::Transverse,
        Orientation::Rotate270,
    ] {
        let host = host_like_orientation(&host_image, orientation);
        let host_rgba = host.to_rgba32f().into_raw();
        let extracted = rapidraw_develop::apply_orientation(&crate_image, orientation);
        let crate_rgba = extracted.rgba32f();
        assert_eq!(host_rgba.len(), crate_rgba.len(), "{orientation:?}");
        assert!(
            host_rgba == crate_rgba,
            "{orientation:?}: crate orientation diverges from host semantics"
        );
    }
}

#[test]
fn geometry_flip_composition_matches_host_image_crate_semantics() {
    let host_image =
        image::DynamicImage::ImageRgba32F(image::ImageBuffer::from_fn(4, 7, |x, y| {
            let v = (y * 4 + x) as f32 * 0.211 + 0.03;
            image::Rgba([v, v, v, 1.0])
        }));
    let crate_image = rapidraw_develop::LinearImage::from_fn(4, 7, |x, y| {
        let v = (y * 4 + x) as f32 * 0.211 + 0.03;
        [v, v, v]
    });

    for (horizontal, vertical) in [(true, false), (false, true), (true, true)] {
        let mut host = host_image.clone();
        if horizontal {
            host = host.fliph();
        }
        if vertical {
            host = host.flipv();
        }
        let extracted = rapidraw_develop::apply_flip(&crate_image, horizontal, vertical);
        assert_eq!(
            host.to_rgba32f().into_raw(),
            extracted.rgba32f(),
            "flip h={horizontal} v={vertical} diverges"
        );
    }
}

#[test]
fn parity_bayer_corpus_cr3_fast_demosaic() {
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
    let extracted = decode_original(
        &bytes,
        &DecodeOptions {
            fast_demosaic: true,
            ..DecodeOptions::default()
        },
    )
    .expect("crate fast decode");
    assert_eq!((host.width(), host.height()), extracted.image.dimensions());
    // Pinned pre-extraction behavior: superpixel demosaic plus the rescaled
    // default crop yield 1367x911 (not a clean quarter of 5472x3648).
    assert_eq!(extracted.image.dimensions(), (1367, 911));
    assert_eq!(host.to_rgba32f().into_raw(), extracted.image.rgba32f());
}
