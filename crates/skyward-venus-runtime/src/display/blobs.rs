//! Pinned Venus msm_drm_pp.h byte layouts, never submitted to DRM here.
use anyhow::{Result, ensure};
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn channel(values: &[u32], count: usize, max: u32) -> Result<()> {
    ensure!(values.len() == count, "unexpected gamma sample count");
    ensure!(
        values.iter().all(|v| *v <= max),
        "gamma sample exceeds hardware width"
    );
    Ok(())
}
pub fn igc(r: &[u32], g: &[u32], b: &[u32], dither: bool, strength: u32) -> Result<Vec<u8>> {
    for values in [r, g, b] {
        channel(values, 257, 4095)?;
    }
    ensure!(strength <= 15, "IGC strength exceeds four bits");
    let mut out = vec![0; 3096];
    out[..8].copy_from_slice(&(dither as u64).to_le_bytes());
    put32(&mut out, 3080, strength);
    for (c, values) in [g, b, r].iter().enumerate() {
        for (i, value) in values[..256].iter().enumerate() {
            put32(&mut out, 8 + c * 1024 + i * 4, *value);
        }
        put32(&mut out, 3084 + c * 4, values[256]);
    }
    Ok(out)
}
pub fn gc(r: &[u32], g: &[u32], b: &[u32]) -> Result<Vec<u8>> {
    for values in [r, g, b] {
        channel(values, 1024, 1023)?;
    }
    let mut out = vec![0; 6152];
    for (c, values) in [g, b, r].iter().enumerate() {
        for i in 0..512 {
            put32(
                &mut out,
                8 + c * 2048 + i * 4,
                values[i * 2] | (values[i * 2 + 1] << 16),
            );
        }
    }
    Ok(out)
}
#[repr(C)]
pub struct SdeGamut {
    pub mode: u32,
    pub map_en: u32,
    pub sizes: [u32; 4],
    pub c0: [*mut u32; 4],
    pub c12: [*mut u32; 4],
    pub scale_sizes: [u32; 3],
    pub scale: [*mut u32; 3],
}
pub struct OwnedGamut {
    pub config: SdeGamut,
    pub c0: Box<[[u32; 1229]; 4]>,
    pub c12: Box<[[u32; 1229]; 4]>,
    pub scale: Box<[[u32; 16]; 3]>,
}
impl OwnedGamut {
    pub fn new() -> Self {
        let mut c0 = Box::new([[0; 1229]; 4]);
        let mut c12 = Box::new([[0; 1229]; 4]);
        let mut scale = Box::new([[0; 16]; 3]);
        let config = SdeGamut {
            mode: 1,
            map_en: 0,
            sizes: [1229; 4],
            c0: std::array::from_fn(|i| c0[i].as_mut_ptr()),
            c12: std::array::from_fn(|i| c12[i].as_mut_ptr()),
            scale_sizes: [16; 3],
            scale: std::array::from_fn(|i| scale[i].as_mut_ptr()),
        };
        Self {
            config,
            c0,
            c12,
            scale,
        }
    }
    pub fn pack(&self) -> Result<Vec<u8>> {
        let cfg = &self.config;
        ensure!(cfg.mode == 1 && cfg.map_en <= 1, "unsupported gamut mode");
        let mut out = vec![0; 39536];
        out[..8].copy_from_slice(&(cfg.map_en as u64).to_le_bytes());
        put32(&mut out, 8, 1);
        for t in 0..4 {
            ensure!(
                cfg.sizes[t] == 1229
                    && std::ptr::eq(cfg.c0[t], self.c0[t].as_ptr())
                    && std::ptr::eq(cfg.c12[t], self.c12[t].as_ptr()),
                "vendor changed owned gamut buffer bounds"
            );
            for i in 0..1229 {
                let at = 204 + (t * 1229 + i) * 8;
                put32(&mut out, at, self.c12[t][i]);
                put32(&mut out, at + 4, self.c0[t][i]);
            }
        }
        for t in 0..3 {
            ensure!(
                cfg.scale_sizes[t] == 16 && std::ptr::eq(cfg.scale[t], self.scale[t].as_ptr()),
                "vendor changed owned gamut scale bounds"
            );
            if cfg.map_en != 0 {
                for (i, value) in self.scale[t].iter().enumerate() {
                    put32(&mut out, 12 + (t * 16 + i) * 4, *value);
                }
            }
        }
        Ok(out)
    }
}
const _: () = {
    assert!(size_of::<SdeGamut>() == 128);
    assert!(std::mem::offset_of!(SdeGamut, scale) == 104);
};
#[cfg(test)]
mod tests {
    use super::*;
    fn at(bytes: &[u8], n: usize) -> u32 {
        u32::from_le_bytes(bytes[n..n + 4].try_into().unwrap())
    }
    #[test]
    fn hardware_channel_order_last_samples_and_bounds() {
        let mut r = vec![100; 257];
        let mut g = vec![200; 257];
        let mut b = vec![300; 257];
        r[256] = 111;
        g[256] = 222;
        b[256] = 333;
        let i = igc(&r, &g, &b, true, 7).unwrap();
        assert_eq!(i.len(), 3096);
        assert_eq!(at(&i, 0), 1);
        assert_eq!(at(&i, 8), 200);
        assert_eq!(at(&i, 3080), 7);
        assert_eq!([at(&i, 3084), at(&i, 3088), at(&i, 3092)], [222, 333, 111]);
        assert!(igc(&r[..256], &g, &b, false, 0).is_err());
        r[256] = 4096;
        assert!(igc(&r, &g, &b, false, 0).is_err());
        r[256] = 111;
        assert!(igc(&r, &g, &b, true, 16).is_err());
        r.resize(1024, 10);
        g.resize(1024, 20);
        b.resize(1024, 30);
        r[1022] = 11;
        r[1023] = 12;
        g[0] = 21;
        g[1] = 22;
        let c = gc(&r, &g, &b).unwrap();
        assert_eq!(c.len(), 6152);
        assert_eq!(at(&c, 8), 21 | (22 << 16));
        assert_eq!(at(&c, 8 + 4096 + 511 * 4), 11 | (12 << 16));
        let mut too_long = g.clone();
        too_long.push(0);
        assert!(gc(&r, &too_long, &b).is_err());
        b[31] = 1024;
        assert!(gc(&r, &g, &b).is_err());
    }
    #[test]
    fn gamut_bounds_ownership_and_padding() {
        let mut owned = OwnedGamut::new();
        owned.config.map_en = 1;
        owned.c0[3][1228] = 0xabc;
        owned.c12[3][1228] = 0x1230456;
        owned.scale[2][15] = 77;
        let bytes = owned.pack().unwrap();
        assert_eq!(at(&bytes, 204 + (3 * 1229 + 1228) * 8), 0x1230456);
        assert_eq!(at(&bytes, 208 + (3 * 1229 + 1228) * 8), 0xabc);
        assert_eq!(at(&bytes, 12 + (2 * 16 + 15) * 4), 77);
        assert_eq!(&bytes[39532..], &[0; 4]);
        owned.config.map_en = 0;
        assert_eq!(at(&owned.pack().unwrap(), 12 + (2 * 16 + 15) * 4), 0);
        owned.config.c0[0] = owned.c0[1].as_mut_ptr();
        assert!(owned.pack().is_err());
        owned.config.c0[0] = owned.c0[0].as_mut_ptr();
        owned.config.sizes[3] = 1230;
        assert!(owned.pack().is_err());
        owned.config.sizes[3] = 1229;
        owned.config.mode = 2;
        assert!(owned.pack().is_err());
    }
}
