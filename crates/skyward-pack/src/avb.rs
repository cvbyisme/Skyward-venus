//! Shared AVB footer implementation, also used by the on-device capsule installer.
pub use skyward_bootimg::avb::footed;

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: [u8; 32] = [0x5a; 32];

    /// What avbtool 1.4.0 makes of `len` bytes of `S`, given by
    /// `add_hash_footer --partition_size 81920 --partition_name recovery
    /// --algorithm NONE --salt 5a…5a --internal_release_string skyward-pack`.
    fn avbtool(len: usize) -> &'static [u8] {
        match len {
            4096 => include_bytes!("../tests/avb-footer-4096.bin"),
            1000 => include_bytes!("../tests/avb-footer-1000.bin"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn the_footer_is_avbtools_byte_for_byte() {
        for len in [4096, 1000] {
            let got = footed(&vec![b'S'; len], "recovery", 81920, &SALT).unwrap();
            assert!(got == avbtool(len), "a {len}-byte image");
        }
    }

    #[test]
    fn an_image_that_leaves_no_room_for_its_vbmeta_and_footer_is_refused() {
        let image = vec![b'S'; 8192];
        assert!(footed(&image, "recovery", 16384, &SALT).is_ok(), "a block each for the vbmeta image and the footer");
        assert!(footed(&image, "recovery", 12288, &SALT).is_err());
    }
}
