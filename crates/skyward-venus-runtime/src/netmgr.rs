use crate::common::*;
use anyhow::{Result, ensure};
use std::{
    ffi::{CStr, CString},
    fs::File,
    io::Write,
    os::fd::FromRawFd,
    path::Path,
};
pub fn ready() -> Result<()> {
    let fd = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    cv(fd)?;
    let mut result = unsafe { File::from_raw_fd(fd) };
    cv(unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) })?;
    result.write_all(if ping(Path::new("/vendor/lib64/libnetmgr.so"))? {
        b"present\n"
    } else {
        b"absent\n"
    })?;
    Ok(())
}
fn ping(path: &Path) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes())?;
    let library = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    ensure!(!library.is_null(), "netmgr-ready dlopen: {}", dlerror());
    unsafe {
        libc::dlerror();
    }
    let symbol = unsafe { libc::dlsym(library, c"netmgr_client_send_ping_msg".as_ptr()) };
    ensure!(!symbol.is_null(), "netmgr-ready dlsym: {}", dlerror());
    // Exact installed libnetmgr C ABI. The owner bounds this process's life.
    let ping: unsafe extern "C" fn(libc::c_int) -> libc::c_int =
        unsafe { std::mem::transmute(symbol) };
    // Deliberately keep the vendor library loaded until process exit: it may
    // retain internal callbacks/threads beyond the synchronous readiness ping.
    Ok(unsafe { ping(500) } == 0)
}
fn dlerror() -> String {
    let p = unsafe { libc::dlerror() };
    if p.is_null() {
        "unknown loader error".into()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("fixture.rs");
        let lib = dir.path().join("libnetmgr.so");
        std::fs::write(&src, source).unwrap();
        assert!(
            std::process::Command::new("rustc")
                .args(["--edition=2024", "--crate-type=cdylib"])
                .arg(src)
                .arg("-o")
                .arg(&lib)
                .status()
                .unwrap()
                .success()
        );
        (dir, lib)
    }
    #[test]
    fn installed_c_abi_uses_500ms_and_only_zero_means_present() {
        let (_dir, path) = fixture(
            "#[unsafe(no_mangle)] pub extern \"C\" fn netmgr_client_send_ping_msg(timeout: i32) -> i32 { if timeout == 500 { 0 } else { -1 } }",
        );
        assert!(ping(&path).unwrap());
        for value in [-1, 1, 14] {
            let (_dir, path) = fixture(&format!(
                "#[unsafe(no_mangle)] pub extern \"C\" fn netmgr_client_send_ping_msg(_: i32) -> i32 {{ {value} }}"
            ));
            assert!(!ping(&path).unwrap());
        }
    }
    #[test]
    fn loader_failures_do_not_become_absent() {
        let (dir, path) =
            fixture("#[unsafe(no_mangle)] pub extern \"C\" fn another_symbol() -> i32 { 0 }");
        assert!(ping(&path).is_err());
        assert!(ping(&dir.path().join("missing.so")).is_err());
    }
}
