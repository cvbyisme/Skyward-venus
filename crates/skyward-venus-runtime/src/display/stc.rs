//! Verified Venus V2 STC user ABI, implemented without a C++ shim.
//!
//! Everything here is a *read-only diagnostic*. Only the 35 SHA256-pinned
//! vendor objects are trusted to provide STL memory layouts. The device's
//! original libraries own all returned vectors/shared_ptr objects for the
//! lifetime of this one-shot process; Rust never frees a vendor allocation.
//! ABI constraints: AArch64, little endian, Android 11 platform libc++ __1
//! default string layout, and the exact offsets validated by vendor symbols.
use super::blobs::{self, OwnedGamut, SdeGamut};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CStr, CString, c_void},
    fs::File,
    io::Read,
    mem,
    path::Path,
};

const PINS: &str = include_str!("../../../../porting/venus/display/vendor-pins.json");
const PANEL: &str = "xiaomi_38_08_0a_cmd_mode_dsc_dsi_panel";
const COLOR: &str = "/vendor/lib64/libsdm-color.so";
const GAMUT_SYMBOL: &str =
    "_ZN3sdm11PPAlgorithm17ConvertGamutCoeffEP10lut3d_infoPNS_11SDEGamutCfgE";

#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct CxxString([u8; 24]);
impl CxxString {
    fn input(bytes: &[u8], backing: &mut Vec<Vec<u8>>) -> Result<Self> {
        ensure!(bytes.len() <= 4096, "platform string length exceeds 4096");
        let mut raw = [0u8; 24];
        if bytes.len() <= 22 {
            raw[0] = (bytes.len() as u8) << 1;
            raw[1..1 + bytes.len()].copy_from_slice(bytes);
        } else {
            // The default (non-alternate) libc++ layout stores cap|1,
            // length and a pointer at +0,+8,+16 on little-endian ARM64.
            let capacity = (bytes.len() + 2).next_multiple_of(2);
            let mut buffer = vec![0u8; capacity];
            buffer[..bytes.len()].copy_from_slice(bytes);
            raw[..8].copy_from_slice(&((capacity as u64) | 1).to_le_bytes());
            raw[8..16].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
            raw[16..24].copy_from_slice(&(buffer.as_ptr() as u64).to_le_bytes());
            backing.push(buffer);
        }
        Ok(Self(raw))
    }
    // SAFETY: all returned strings come from fixed-size members of verified
    // vendor C++ allocations, not from untrusted shell, device, or network.
    unsafe fn read(&self) -> Result<String> {
        let (source, len): (*const u8, usize) = if self.0[0] & 1 == 0 {
            let len = (self.0[0] >> 1) as usize;
            ensure!(len <= 22, "invalid inline libc++ string size");
            (self.0[1..].as_ptr(), len)
        } else {
            let cap = (u64::from_le_bytes(self.0[..8].try_into()?) & !1) as usize;
            let len = u64::from_le_bytes(self.0[8..16].try_into()?) as usize;
            let ptr = u64::from_le_bytes(self.0[16..24].try_into()?) as *const u8;
            ensure!(
                len <= 4096 && cap > len && !ptr.is_null(),
                "invalid long libc++ string"
            );
            (ptr, len)
        };
        Ok(
            String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(source, len) })
                .into_owned(),
        )
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CxxVector {
    begin: *const u8,
    end: *const u8,
    capacity: *const u8,
}
impl CxxVector {
    unsafe fn read(raw: *const u8) -> Self {
        unsafe { raw.cast::<Self>().read_unaligned() }
    }
    fn items(self, width: usize, max: usize) -> Result<(*const u8, usize)> {
        let (begin, end, cap) = (
            self.begin as usize,
            self.end as usize,
            self.capacity as usize,
        );
        if begin == 0 {
            ensure!(end == 0 && cap == 0, "mismatched empty libc++ vector");
            return Ok((self.begin, 0));
        }
        ensure!(
            width > 0 && begin <= end && end <= cap && begin % 8 == 0,
            "invalid libc++ vector pointers"
        );
        let used = end - begin;
        ensure!(
            used % width == 0 && used / width <= max,
            "unbounded libc++ vector"
        );
        Ok((self.begin, used / width))
    }
}

pub(super) struct ColorMode {
    ptr: *const u8,
    gamut: u32,
    gamma: u32,
    intent: u32,
    name: String,
    assets: Vec<String>,
}
impl ColorMode {
    unsafe fn read(ptr: *const u8) -> Result<Self> {
        let int = |offset| unsafe { ptr.add(offset).cast::<u32>().read_unaligned() };
        let vector = unsafe { CxxVector::read(ptr.add(16)) };
        let (base, count) = vector.items(24, 64)?;
        let mut assets = Vec::with_capacity(count);
        for i in 0..count {
            let word = unsafe { base.add(i * 24).cast::<CxxString>().read_unaligned() };
            assets.push(unsafe { word.read() }?);
        }
        let name = unsafe { ptr.add(40).cast::<CxxString>().read_unaligned() };
        Ok(Self {
            ptr,
            gamut: int(0),
            gamma: int(4),
            intent: int(8),
            name: unsafe { name.read() }?,
            assets,
        })
    }
}
const _: () = {
    assert!(mem::size_of::<CxxString>() == 24);
    assert!(mem::size_of::<CxxVector>() == 24);
    assert!(mem::size_of::<SdeGamut>() == 128);
};

#[repr(C, align(8))]
pub(super) struct PlatformBytes<const N: usize>([u8; N]);
impl<const N: usize> PlatformBytes<N> {
    fn new(version: u32) -> Self {
        let mut output = Self([0; N]);
        output.0[..4].copy_from_slice(&version.to_le_bytes());
        output
    }
    fn vec(&self, at: usize) -> CxxVector {
        unsafe { CxxVector::read(self.0.as_ptr().add(at)) }
    }
}
fn payload<T>(kind: u32, value: &mut T) -> PlatformBytes<24> {
    let mut out = PlatformBytes::new(24);
    out.0[4..8].copy_from_slice(&(mem::size_of::<T>() as u32).to_le_bytes());
    out.0[8..12].copy_from_slice(&kind.to_le_bytes());
    out.0[16..24].copy_from_slice(&(value as *mut T as u64).to_le_bytes());
    out
}
struct ModeInput {
    raw: PlatformBytes<2912>,
    // Keep every Rust allocation backing the C++ view alive until ProcessOps
    // returns. No vendor function ever owns this caller-created input.
    _asset_values: Vec<CxxString>,
    _strings: Vec<Vec<u8>>,
}
impl ModeInput {
    unsafe fn new(mode: &ColorMode, id: u32) -> Result<Self> {
        let mut heap = Vec::new();
        let mut assets = Vec::with_capacity(mode.assets.len());
        for name in &mode.assets {
            assets.push(CxxString::input(name.as_bytes(), &mut heap)?);
        }
        let name = CxxString::input(mode.name.as_bytes(), &mut heap)?;
        let mut raw = PlatformBytes::new(2912);
        raw.0[32..44].copy_from_slice(unsafe { std::slice::from_raw_parts(mode.ptr, 12) });
        if !assets.is_empty() {
            let begin = assets.as_ptr() as u64;
            let end = begin + (assets.len() * 24) as u64;
            for (at, value) in [(48, begin), (56, end), (64, end)] {
                raw.0[at..at + 8].copy_from_slice(&value.to_le_bytes());
            }
        }
        raw.0[72..96].copy_from_slice(&name.0);
        raw.0[2904..2908].copy_from_slice(&id.to_le_bytes());
        Ok(Self {
            raw,
            _asset_values: assets,
            _strings: heap,
        })
    }
}

pub(super) struct Library(*mut c_void);
impl Library {
    pub fn load(path: &str) -> Result<Self> {
        let name = CString::new(path)?;
        let lib = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        ensure!(
            !lib.is_null(),
            "vendor library {} failed: {}",
            path,
            dlerror()
        );
        Ok(Self(lib))
    }
    pub fn symbol(&self, name: &str) -> Result<*mut c_void> {
        let name = CString::new(name)?;
        let ptr = unsafe { libc::dlsym(self.0, name.as_ptr()) };
        ensure!(
            !ptr.is_null(),
            "vendor symbol {} failed: {}",
            name.to_string_lossy(),
            dlerror()
        );
        Ok(ptr)
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { libc::dlclose(self.0) };
        }
    }
}
fn dlerror() -> String {
    let p = unsafe { libc::dlerror() };
    if p.is_null() {
        "unknown dynamic linker failure".into()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

pub(super) struct Interface {
    object: *mut c_void,
    initialized: bool,
}
impl Interface {
    pub fn create(manager: &Library) -> Result<Self> {
        let entry = manager.symbol("GetScPostBlendInterface")?;
        let factory: unsafe extern "C" fn(u32, u32) -> *mut c_void =
            unsafe { mem::transmute(entry) };
        let object = unsafe { factory(2, 0) };
        ensure!(!object.is_null(), "STC V2 interface factory returned null");
        Ok(Self {
            object,
            initialized: false,
        })
    }
    unsafe fn method(&self, at: usize) -> *mut c_void {
        let vtable = unsafe { (self.object as *const *const *mut c_void).read() };
        unsafe { *vtable.add(at) }
    }
    pub fn initialize(&mut self) -> Result<()> {
        let mut backing = Vec::new();
        let panel = CxxString::input(PANEL.as_bytes(), &mut backing)?;
        let call: unsafe extern "C" fn(*mut c_void, *const CxxString) -> i32 =
            unsafe { mem::transmute(self.method(2)) };
        let result = unsafe { call(self.object, &panel) };
        ensure!(result == 0, "STC Init failed: {result}");
        self.initialized = true;
        Ok(())
    }
    pub fn modes(&self) -> Result<(PlatformBytes<32>, Vec<ColorMode>)> {
        let mut result = PlatformBytes::<32>::new(96);
        let mut call_arg = payload(1, &mut result);
        let call: unsafe extern "C" fn(*mut c_void, *mut PlatformBytes<24>) -> i32 =
            unsafe { mem::transmute(self.method(5)) };
        ensure!(
            unsafe { call(self.object, &mut call_arg) } == 0,
            "STC mode enumeration failed"
        );
        ensure!(
            u32::from_le_bytes(result.0[..4].try_into()?) == 96,
            "invalid STC list version"
        );
        let (base, count) = result.vec(8).items(64, 256)?;
        ensure!(count > 0, "empty STC mode list");
        let mut modes = Vec::with_capacity(count);
        for i in 0..count {
            modes.push(unsafe { ColorMode::read(base.add(i * 64)) }?);
        }
        Ok((result, modes))
    }
    pub fn render(&self, mode: &ColorMode, index: usize, id: u32) -> Result<Vec<Record>> {
        let mut params = unsafe { ModeInput::new(mode, id) }?;
        let mut out = PlatformBytes::<32>::new(32);
        let input = payload(3, &mut params.raw);
        let mut output = payload(4, &mut out);
        let call: unsafe extern "C" fn(
            *mut c_void,
            u32,
            *const PlatformBytes<24>,
            *mut PlatformBytes<24>,
        ) -> i32 = unsafe { mem::transmute(self.method(6)) };
        ensure!(
            unsafe { call(self.object, 0, &input, &mut output) } == 0,
            "STC ProcessOps render failed"
        );
        ensure!(
            u32::from_le_bytes(out.0[..4].try_into()?) == 32,
            "invalid STC render output version"
        );
        let (base, count) = out.vec(8).items(56, 32)?;
        ensure!(count > 0, "STC returned no render assets");
        let mut records = Vec::with_capacity(count);
        let color = Library::load(COLOR)?;
        for i in 0..count {
            let ptr = unsafe { base.add(i * 56) };
            let version = unsafe { ptr.cast::<u32>().read_unaligned() };
            ensure!(version == 56, "unknown STC hw payload ABI");
            let asset = unsafe { ptr.add(8).cast::<CxxString>().read_unaligned() };
            let asset = unsafe { asset.read() }?;
            let data = unsafe { ptr.add(32).cast::<*const u8>().read_unaligned() };
            let len = unsafe { ptr.add(48).cast::<u32>().read_unaligned() };
            ensure!(!data.is_null(), "STC returned null hw payload");
            let record = unsafe { record(&asset, data, len, &color) }?;
            ensure!(
                !records
                    .iter()
                    .any(|r: &Record| r.property == record.property),
                "duplicate STC property"
            );
            records.push(record);
        }
        eprintln!(
            "GENERATED_ONLY mode_index={index} qdcm_mode_id={id} assets={} DRM_NOT_APPLIED",
            records.len()
        );
        Ok(records)
    }
    pub fn deinitialize(&mut self) -> Result<()> {
        if self.initialized {
            let call: unsafe extern "C" fn(*mut c_void) -> i32 =
                unsafe { mem::transmute(self.method(3)) };
            let result = unsafe { call(self.object) };
            self.initialized = false;
            ensure!(result == 0, "STC DeInit failed: {result}");
        }
        Ok(())
    }
}
impl Drop for Interface {
    fn drop(&mut self) {
        if !self.object.is_null() {
            if self.initialized {
                let _ = self.deinitialize();
            }
            // Itanium virtual deleting destructor; entry 1 frees factory object.
            let call: unsafe extern "C" fn(*mut c_void) = unsafe { mem::transmute(self.method(1)) };
            unsafe { call(self.object) };
        }
    }
}

#[derive(Debug)]
pub(super) struct Record {
    pub property: &'static str,
    pub enabled: bool,
    pub bytes: Vec<u8>,
}
unsafe fn vector_u32(ptr: *const u8, offset: usize, max: usize) -> Result<Vec<u32>> {
    let raw = unsafe { CxxVector::read(ptr.add(offset)) };
    let (base, len) = raw.items(4, max)?;
    ensure!(len == max, "gamma sample count mismatch");
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push(unsafe { base.add(i * 4).cast::<u32>().read_unaligned() });
    }
    Ok(out)
}
unsafe fn record(asset: &str, data: *const u8, len: u32, color: &Library) -> Result<Record> {
    let version = unsafe { data.cast::<u32>().read_unaligned() };
    ensure!(version == len, "STC hw payload version disagrees with size");
    let enabled = unsafe { *data.add(4) != 0 };
    let (property, bytes) = match asset {
        "PostBlendIGC" | "PostBlendGC" => {
            ensure!(len == 88, "unsupported gamma ABI size");
            let count = if asset == "PostBlendIGC" { 257 } else { 1024 };
            let bytes = if enabled {
                let r = unsafe { vector_u32(data, 8, count) }?;
                let g = unsafe { vector_u32(data, 32, count) }?;
                let b = unsafe { vector_u32(data, 56, count) }?;
                if count == 257 {
                    let dither = unsafe { *data.add(80) != 0 };
                    let strength = unsafe { data.add(84).cast::<u32>().read_unaligned() };
                    blobs::igc(&r, &g, &b, dither, strength)?
                } else {
                    blobs::gc(&r, &g, &b)?
                }
            } else {
                Vec::new()
            };
            (
                if count == 257 {
                    "SDE_DSPP_IGC_V4"
                } else {
                    "SDE_DSPP_GC_V1"
                },
                bytes,
            )
        }
        "PostBlendGamut" => {
            ensure!(len == 117936, "unsupported 17-cube gamut ABI size");
            let bytes = if enabled {
                let mut lut = vec![0u64; 117928 / 8];
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        data.add(8),
                        lut.as_mut_ptr().cast::<u8>(),
                        117928,
                    )
                };
                let raw = unsafe { std::slice::from_raw_parts(lut.as_ptr().cast::<u8>(), 117928) };
                ensure!(
                    u32::from_le_bytes(raw[12..16].try_into()?) == 4913,
                    "only complete 17x17x17 gamut accepted"
                );
                let mut owned = OwnedGamut::new();
                let sym = color.symbol(GAMUT_SYMBOL)?;
                let convert: unsafe extern "C" fn(*mut c_void, *mut SdeGamut) -> i32 =
                    unsafe { mem::transmute(sym) };
                ensure!(
                    unsafe { convert(lut.as_mut_ptr().cast(), &mut owned.config) } == 0,
                    "vendor gamut coefficient conversion failed"
                );
                owned.pack()?
            } else {
                Vec::new()
            };
            ("SDE_DSPP_GAMUT_V4", bytes)
        }
        other => bail!("unsupported STC hardware asset: {other}"),
    };
    Ok(Record {
        property,
        enabled,
        bytes,
    })
}

pub(super) fn verify_vendor() -> Result<()> {
    let files: std::collections::BTreeMap<String, String> = serde_json::from_str(PINS)?;
    ensure!(
        files.len() == 35,
        "expected exactly 35 pinned vendor inputs"
    );
    let mut buffer = [0u8; 16 * 1024];
    for (path, expected) in files {
        ensure!(
            path.starts_with("/vendor/") && !path.contains("..") && expected.len() == 64,
            "invalid pinned vendor entry"
        );
        let mut file =
            File::open(Path::new(&path)).with_context(|| format!("verify vendor {path}"))?;
        let mut hash = Sha256::new();
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        ensure!(
            format!("{:x}", hash.finalize()) == expected,
            "vendor asset hash mismatch: {path}"
        );
    }
    Ok(())
}

pub(super) fn json_modes(modes: &[ColorMode]) -> Result<Vec<u8>> {
    use serde_json::json;
    let body: Vec<_> = modes
        .iter()
        .enumerate()
        .map(|(index, m)| {
            json!({
                "index":index,"gamut":m.gamut,"gamma":m.gamma,"intent":m.intent,
                "name":m.name,"assets":m.assets
            })
        })
        .collect();
    let mut result = serde_json::to_vec_pretty(&body)?;
    result.push(b'\n');
    Ok(result)
}
pub(super) fn frame(records: &[Record]) -> Result<Vec<u8>> {
    ensure!(
        !records.is_empty() && records.len() <= 3,
        "invalid STC record count"
    );
    let mut out = b"VCL1".to_vec();
    out.extend_from_slice(&(records.len() as u32).to_le_bytes());
    for r in records {
        let name = r.property.as_bytes();
        ensure!(name.len() <= 64, "invalid DRM property name");
        ensure!(
            (r.enabled && !r.bytes.is_empty()) || (!r.enabled && r.bytes.is_empty()),
            "invalid enabled asset"
        );
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(&(r.enabled as u32).to_le_bytes());
        out.extend_from_slice(&(r.bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&r.bytes);
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_vendor_count_and_string_abi() {
        let files: std::collections::BTreeMap<String, String> = serde_json::from_str(PINS).unwrap();
        assert_eq!(files.len(), 35);
        for value in ["", "test", "1234567890123456789012", PANEL] {
            let mut backing = Vec::new();
            let s = CxxString::input(value.as_bytes(), &mut backing).unwrap();
            assert_eq!(unsafe { s.read() }.unwrap(), value);
            assert_eq!(s.0[0] & 1, if value.len() > 22 { 1 } else { 0 });
        }
        assert_eq!(mem::size_of::<PlatformBytes<2912>>(), 2912);
        assert_eq!(mem::align_of::<PlatformBytes<2912>>(), 8);
    }
    #[test]
    fn exact_mode_render_input_offsets() {
        let mut original = [0u8; 64];
        original[..4].copy_from_slice(&7u32.to_le_bytes());
        original[4..8].copy_from_slice(&8u32.to_le_bytes());
        original[8..12].copy_from_slice(&9u32.to_le_bytes());
        let mode = ColorMode {
            ptr: original.as_ptr(),
            gamut: 7,
            gamma: 8,
            intent: 9,
            name: "long_display_name_for_venus".into(),
            assets: vec!["igc".into()],
        };
        let input = unsafe { ModeInput::new(&mode, 17).unwrap() };
        assert_eq!(&input.raw.0[32..44], &original[..12]);
        assert_eq!(
            u32::from_le_bytes(input.raw.0[2904..2908].try_into().unwrap()),
            17
        );
        let name = unsafe {
            input.raw.0[72..]
                .as_ptr()
                .cast::<CxxString>()
                .read_unaligned()
        };
        assert_eq!(unsafe { name.read() }.unwrap(), mode.name);
        let (_, count) = input.raw.vec(48).items(24, 64).unwrap();
        assert_eq!(count, 1);
        let mut output = PlatformBytes::<32>::new(32);
        let p = payload(4, &mut output);
        assert_eq!(u32::from_le_bytes(p.0[4..8].try_into().unwrap()), 32);
    }
    #[test]
    fn frame_preserves_enabled_flag_and_rejects_ambiguous_records() {
        let a = Record {
            property: "SDE_DSPP_GC_V1",
            enabled: false,
            bytes: Vec::new(),
        };
        let raw = frame(&[a]).unwrap();
        assert_eq!(&raw[..4], b"VCL1");
        assert_eq!(&raw[12..16], &0u32.to_le_bytes());
        let malformed = Record {
            property: "SDE_DSPP_GC_V1",
            enabled: true,
            bytes: Vec::new(),
        };
        assert!(frame(&[malformed]).is_err());
    }
}
