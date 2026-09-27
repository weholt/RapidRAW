//! White-balance policy and multi-exposure neutralization.
//!
//! The decode boundary treats white balance as an explicit input: the source
//! file's coefficients are used as-is ([`WhiteBalancePolicy::FromFile`]) or
//! neutralized for in-camera multi-exposure captures
//! ([`WhiteBalancePolicy::AutoNeutralizeMultiExposure`], the host's historic
//! behavior), or always forced to neutral ([`WhiteBalancePolicy::Neutral`]).
//!
//! `neutralize_wb_if_multiexposure` is a faithful port of the host's
//! `multi_exposure::neutralize_wb_if_multiexposure`: for Canon CR2-style
//! containers with an in-camera multi-exposure maker-note flag, every finite
//! coefficient is replaced by `1.0`.

/// Explicit white-balance interpretation policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhiteBalancePolicy {
    /// Use the coefficients from the file unmodified.
    FromFile,
    /// Neutralize the white balance when the file is an in-camera
    /// multi-exposure capture (Canon maker-note flag). This mirrors the
    /// RapidRAW host behavior the extraction must preserve.
    #[default]
    AutoNeutralizeMultiExposure,
    /// Always neutralize: every finite coefficient becomes `1.0`.
    Neutral,
}

/// Neutralize the white balance according to `policy`. Returns the effective
/// coefficients and whether neutralization fired.
pub fn neutralize_wb_if_multiexposure(
    wb_coeffs: [f32; 4],
    file_bytes: &[u8],
    policy: WhiteBalancePolicy,
) -> ([f32; 4], bool) {
    match policy {
        WhiteBalancePolicy::FromFile => (wb_coeffs, false),
        WhiteBalancePolicy::Neutral => {
            let mut neutralized = wb_coeffs;
            for coeff in &mut neutralized {
                if coeff.is_finite() {
                    *coeff = 1.0;
                }
            }
            (neutralized, true)
        }
        WhiteBalancePolicy::AutoNeutralizeMultiExposure => {
            if is_incamera_multiexposure_canon(file_bytes) {
                log::info!("[raw_hdr_wb] multi-exposure CR2 detected, neutralizing WB");
                let mut neutralized = wb_coeffs;
                for exp in &mut neutralized {
                    if exp.is_finite() {
                        *exp = 1.0;
                    }
                }
                (neutralized, true)
            } else {
                (wb_coeffs, false)
            }
        }
    }
}

fn is_incamera_multiexposure_canon(file_bytes: &[u8]) -> bool {
    assert!(file_bytes.len() >= 8, "CR2 file must be at least 8 bytes");

    match file_bytes.get(0..4) {
        Some([0x49, 0x49, 0x2A, 0x00]) => {}
        _ => return false,
    }

    let walk = || -> Option<bool> {
        let b: [u8; 4] = file_bytes.get(4..8)?.try_into().ok()?;
        let ifd0_offset = u32::from_le_bytes(b) as usize;
        let exif_ifd_offset = _find_ifd_entry(file_bytes, ifd0_offset, 0x8769)? as usize;
        let maker_note_offset = _find_ifd_entry(file_bytes, exif_ifd_offset, 0x927C)? as usize;
        let multi_exp_block_offset =
            _find_ifd_entry(file_bytes, maker_note_offset, 0x4021)? as usize;
        let flag_offset = multi_exp_block_offset + 4;
        let v: [u8; 4] = file_bytes
            .get(flag_offset..flag_offset + 4)?
            .try_into()
            .ok()?;
        Some(u32::from_le_bytes(v) == 1)
    };

    walk().unwrap_or(false)
}

fn _find_ifd_entry(file_bytes: &[u8], ifd_offset: usize, tag_id: u16) -> Option<u32> {
    let rd16 = |offset: usize| -> Option<u16> {
        let b: [u8; 2] = file_bytes.get(offset..offset + 2)?.try_into().ok()?;
        Some(u16::from_le_bytes(b))
    };

    let rd32 = |offset: usize| -> Option<u32> {
        let b: [u8; 4] = file_bytes.get(offset..offset + 4)?.try_into().ok()?;
        Some(u32::from_le_bytes(b))
    };

    let entry_count = rd16(ifd_offset)? as usize;
    let capped_count = entry_count.min(512);

    for i in 0..capped_count {
        let entry_offset = ifd_offset + 2 + i * 12;
        let tag = rd16(entry_offset)?;

        if tag == tag_id {
            return rd32(entry_offset + 8);
        }
    }

    None
}
