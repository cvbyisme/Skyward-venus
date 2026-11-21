//! Android 11 *platform* Binder singleton preparation before vendor dlopen.
//! The process uses Rust for all application logic but vendor STC expects
//! Android libbinder's ProcessState to exist. Call the matched exported C++
//! symbol through the verified AArch64 ABI; never build our own C++ wrapper.
use anyhow::{Context, Result, ensure};
#[cfg(target_arch = "aarch64")]
use std::os::unix::fs::FileTypeExt;
use std::{
    ffi::{CStr, c_void},
    os::fd::RawFd,
};

const LIBBINDER: &str = "/apex/com.android.vndk.v30/lib64/libbinder.so";

pub(super) struct ProcessState {
    library: *mut c_void,
    pointer: *mut c_void,
}

impl ProcessState {
    #[cfg(target_arch = "aarch64")]
    pub fn prepare() -> Result<Self> {
        let metadata = std::fs::metadata("/dev/vndbinder")?;
        ensure!(
            metadata.file_type().is_char_device(),
            "private vendor Binder must be character inode"
        );
        let library = std::ffi::CString::new(LIBBINDER)?;
        let handle = unsafe { libc::dlopen(library.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        ensure!(
            !handle.is_null(),
            "platform libbinder dlopen: {}",
            dlerror()
        );
        let func = symbol(handle, c"_ZN7android12ProcessState14initWithDriverEPKc")?;
        let name = c"/dev/vndbinder";
        // arm64 Itanium ABI: sp<ProcessState> has a nontrivial destructor,
        // so the implicit object-return location is x8, never x0.
        let mut returned: *mut c_void = std::ptr::null_mut();
        unsafe {
            core::arch::asm!("blr {entry}", entry=in(reg) func,
                in("x0") name.as_ptr(), in("x8") &mut returned as *mut *mut c_void,
                clobber_abi("C"));
        }
        ensure!(
            !returned.is_null(),
            "platform ProcessState returned no object"
        );
        Ok(Self {
            library: handle,
            pointer: returned,
        })
    }
    #[cfg(not(target_arch = "aarch64"))]
    pub fn prepare() -> Result<Self> {
        anyhow::bail!("platform vendor Binder only on ARM64")
    }
    pub fn start_threads(&self) -> Result<()> {
        let method = symbol(
            self.library,
            c"_ZN7android12ProcessState15startThreadPoolEv",
        )?;
        let run: unsafe extern "C" fn(*mut c_void) = unsafe { std::mem::transmute(method) };
        unsafe { run(self.pointer) };
        Ok(())
    }
}
// Intentionally leak one platform sp<> reference. The STC process exits
// after a single experiment: unloading libbinder before plugin callbacks end
// would be invalid. There is no destructor ABI cross-call to Rust here.
impl Drop for ProcessState {
    fn drop(&mut self) {
        let _ = self.library;
    }
}
fn dlerror() -> String {
    let p = unsafe { libc::dlerror() };
    if p.is_null() {
        "unknown platform loader error".into()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}
fn symbol(lib: *mut c_void, name: &CStr) -> Result<*mut c_void> {
    let ptr = unsafe { libc::dlsym(lib, name.as_ptr()) };
    ensure!(
        !ptr.is_null(),
        "missing VNDK Binder symbol {}: {}",
        name.to_string_lossy(),
        dlerror()
    );
    Ok(ptr)
}
pub(super) fn bound_fd(except: Option<RawFd>) -> Result<RawFd> {
    use std::{mem::MaybeUninit, os::unix::fs::MetadataExt};
    let expected = std::fs::metadata("/dev/vndbinder")?.rdev();
    let mut found = None;
    for fd in 3..1024 {
        if Some(fd) == except {
            continue;
        }
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } == 0 {
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT == libc::S_IFCHR && stat.st_rdev == expected {
                ensure!(
                    found.is_none(),
                    "ambiguous vendor Binder driver descriptors"
                );
                found = Some(fd);
            }
        }
    }
    found.context("platform ProcessState has no pinned vndbinder descriptor")
}
#[cfg(test)]
mod tests {
    #[test]
    fn process_state_symbols_are_from_verified_vndk30() {
        assert_eq!(
            super::LIBBINDER,
            "/apex/com.android.vndk.v30/lib64/libbinder.so"
        );
    }
}
