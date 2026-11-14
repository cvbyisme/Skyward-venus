//! Temporary venus probe only: retain the recovery root on `fastboot boot`.
//!
//! The stock init in this exact image inlines the force_normal_boot search
//! twice. The text is synthesized by MOV/MOVK instructions, not present as
//! a contiguous string. At the first site the recovery module-list decision
//! becomes IsRecoveryMode(); at the second the switch-root block is skipped.
//! Only the conditional jumps into the searches are replaced with NOPs.
//! No kernel code, mount implementation, SELinux code, or boot parameters
//! are patched. Never apply this to a daily-use boot image.
use anyhow::{ensure, Result};
use sha2::{Digest, Sha256};

pub const SOURCE_SHA256: &str = "38429485d5212ec36624e8d12808c83544f3ce41fcad21f07f5f29075b1b6d9b";
pub const PATCHED_SHA256: &str = "047f6a7463610712ba6e63c91e7ad0f1c407a4cef7323fcac4aad109631ecabf";
const SOURCE_LEN: usize = 1_080_648;
const NOP: [u8; 4] = [0x1f, 0x20, 0x03, 0xd5];

// File offsets equal virtual addresses in the inspected executable PT_LOAD.
// 0x77ab4: b.ge 0x77be0; fall through to the not-found result (recovery=true).
// 0x77d28: b.ge 0x77e14; fall through to the no-switch-root path at 0x77d2c.
const SITES: [(usize, [u8; 4]); 2] = [(0x77ab4, [0x6a, 0x09, 0x00, 0x54]), (0x77d28, [0x6a, 0x07, 0x00, 0x54])];

pub fn patch(input: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        input.len() == SOURCE_LEN,
        "unsupported venus init length {}; refusing first-stage patch",
        input.len()
    );
    ensure!(
        input.starts_with(b"\x7fELF\x02\x01") && input.get(18..20) == Some(&[0xb7, 0x00]),
        "venus init is not little-endian ELF64 AArch64"
    );
    let digest = format!("{:x}", Sha256::digest(input));
    ensure!(
        digest == SOURCE_SHA256,
        "unsupported venus init SHA-256 {digest}; expected {SOURCE_SHA256}; refusing guessed offsets"
    );
    let mut output = input.to_vec();
    patch_sites(&mut output)?;
    ensure!(
        format!("{:x}", Sha256::digest(&output)) == PATCHED_SHA256,
        "unexpected patched init checksum"
    );
    Ok(output)
}

fn patch_sites(data: &mut [u8]) -> Result<()> {
    // Validate every site before changing any byte.
    for (offset, expected) in SITES {
        ensure!(
            data.get(offset..offset + 4) == Some(expected.as_slice()),
            "unexpected instruction at venus init offset {offset:#x}"
        );
    }
    for (offset, _) in SITES {
        data[offset..offset + 4].copy_from_slice(&NOP);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut data = vec![0; SOURCE_LEN];
        for (offset, word) in SITES {
            data[offset..offset + 4].copy_from_slice(&word);
        }
        data
    }

    #[test]
    fn exactly_the_two_verified_instructions_change() {
        let before = fixture();
        let mut after = before.clone();
        patch_sites(&mut after).unwrap();
        let changed: Vec<_> = before
            .iter()
            .zip(&after)
            .enumerate()
            .filter_map(|(i, (a, b))| (a != b).then_some(i))
            .collect();
        let expected: Vec<_> = SITES.iter().flat_map(|(offset, _)| *offset..*offset + 4).collect();
        assert_eq!(changed, expected);
        for (offset, _) in SITES {
            assert_eq!(&after[offset..offset + 4], NOP);
        }
    }

    #[test]
    fn a_mismatch_at_the_second_site_changes_nothing() {
        let mut data = fixture();
        data[SITES[1].0] ^= 1;
        let before = data.clone();
        assert!(patch_sites(&mut data).is_err());
        assert_eq!(data, before);
    }

    #[test]
    fn short_and_unknown_binaries_are_refused() {
        assert!(patch(b"not init").is_err());
        let mut unknown = fixture();
        unknown[..6].copy_from_slice(b"\x7fELF\x02\x01");
        unknown[18..20].copy_from_slice(&[0xb7, 0]);
        assert!(patch(&unknown).unwrap_err().to_string().contains("SHA-256"));
    }

    #[test]
    fn applying_twice_is_refused() {
        let mut data = fixture();
        patch_sites(&mut data).unwrap();
        assert!(patch_sites(&mut data).is_err());
    }

    #[test]
    fn original_branches_are_the_verified_search_entries() {
        for ((offset, bytes), target) in SITES.iter().zip([0x77be0_i64, 0x77e14]) {
            let word = u32::from_le_bytes(*bytes);
            assert_eq!(word & 0xff00001f, 0x5400000a, "B.GE");
            let imm19 = ((word >> 5) & 0x7ffff) as i32;
            let signed = (imm19 << 13) >> 13;
            assert_eq!(*offset as i64 + i64::from(signed) * 4, target);
        }
    }
}
