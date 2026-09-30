//! White-balance policy tests, including a synthetic Canon multi-exposure
//! maker-note structure matching the host's IFD walk.

use rapidraw_develop::{WhiteBalancePolicy, neutralize_wb_if_multiexposure};

const COEFFS: [f32; 4] = [2.1, 1.0, 1.0, 1.3];

/// Build a minimal little-endian TIFF: IFD0 -> ExifIFD -> MakerNote ->
/// tag 0x4021 block with a 4-byte flag at offset +4.
fn canon_multiexposure_tiff(flag: u32) -> Vec<u8> {
    // Layout (all offsets absolute; each IFD is count(2) + entry(12) +
    // next-IFD pointer(4) = 18 bytes):
    //  0:  header (8 bytes)
    //  8:  IFD0 (1 entry, tag 0x8769)
    //  26: ExifIFD (1 entry, tag 0x927C)
    //  44: MakerNote IFD (1 entry, tag 0x4021)
    //  62: multi-exposure block (4 undetermined bytes, then the flag)
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00]); // "II*\0"
    buf.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset

    let exif_ifd = 26usize;
    let maker_note = 44usize;
    let multi_exp_block = 62usize;

    // IFD0: entry tag 0x8769 (ExifIFD), type LONG(4), count 1, value offset.
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&0x8769u16.to_le_bytes());
    buf.extend_from_slice(&4u16.to_le_bytes());
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(&(exif_ifd as u32).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());

    // ExifIFD: entry tag 0x927C (MakerNote).
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&0x927Cu16.to_le_bytes());
    buf.extend_from_slice(&4u16.to_le_bytes());
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(&(maker_note as u32).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());

    // MakerNote: entry tag 0x4021 (multi-exposure block).
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&0x4021u16.to_le_bytes());
    buf.extend_from_slice(&4u16.to_le_bytes());
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(&(multi_exp_block as u32).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());

    assert_eq!(buf.len(), multi_exp_block);
    // Multi-exposure block: 4 undetermined bytes, then the flag at +4.
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&flag.to_le_bytes());
    buf
}

#[test]
fn incamera_multiexposure_flag_neutralizes_all_coefficients() {
    let bytes = canon_multiexposure_tiff(1);
    let (neutralized, fired) = neutralize_wb_if_multiexposure(
        COEFFS,
        &bytes,
        WhiteBalancePolicy::AutoNeutralizeMultiExposure,
    );
    assert!(fired, "neutralization must be reported");
    assert_eq!(neutralized, [1.0, 1.0, 1.0, 1.0]);
}

#[test]
fn multiexposure_flag_off_keeps_file_coefficients() {
    let bytes = canon_multiexposure_tiff(0);
    let (kept, fired) = neutralize_wb_if_multiexposure(
        COEFFS,
        &bytes,
        WhiteBalancePolicy::AutoNeutralizeMultiExposure,
    );
    assert!(!fired);
    assert_eq!(kept, COEFFS);
}

#[test]
fn non_tiff_bytes_never_neutralize() {
    let (kept, fired) = neutralize_wb_if_multiexposure(
        COEFFS,
        b"not a tiff at all",
        WhiteBalancePolicy::AutoNeutralizeMultiExposure,
    );
    assert!(!fired);
    assert_eq!(kept, COEFFS);
}

#[test]
fn from_file_policy_never_touches_coefficients() {
    let bytes = canon_multiexposure_tiff(1);
    let (kept, fired) =
        neutralize_wb_if_multiexposure(COEFFS, &bytes, WhiteBalancePolicy::FromFile);
    assert!(!fired, "FromFile must not report neutralization");
    assert_eq!(kept, COEFFS);
}

#[test]
fn neutral_policy_forces_all_coefficients_to_one() {
    let (forced, fired) = neutralize_wb_if_multiexposure(COEFFS, &[], WhiteBalancePolicy::Neutral);
    assert!(fired);
    assert_eq!(forced, [1.0, 1.0, 1.0, 1.0]);
}

#[test]
fn non_finite_coefficients_are_left_during_multiexposure_neutralization() {
    let bytes = canon_multiexposure_tiff(1);
    let (neutralized, fired) = neutralize_wb_if_multiexposure(
        [f32::NAN, 1.0, f32::INFINITY, 2.0],
        &bytes,
        WhiteBalancePolicy::AutoNeutralizeMultiExposure,
    );
    assert!(fired);
    assert!(neutralized[0].is_nan());
    assert_eq!(neutralized[1], 1.0);
    assert!(neutralized[2].is_infinite());
    assert_eq!(neutralized[3], 1.0);
}

#[test]
#[should_panic]
fn short_input_panics_like_the_host_helper() {
    neutralize_wb_if_multiexposure(
        COEFFS,
        &[0x49],
        WhiteBalancePolicy::AutoNeutralizeMultiExposure,
    );
}
