use anyhow::{Result, ensure};
use std::{
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
};

pub fn lock(name: &str) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(format!("/skyward/run/locks/{name}.lock"))?;
    let st = file.metadata()?;
    ensure!(
        st.is_file() && st.uid() == 0 && st.mode() & 0o7777 == 0o600 && st.nlink() == 1,
        "foreign or unsafe service owner lock"
    );
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "cannot exclusively own service lock: {}",
        std::io::Error::last_os_error()
    );
    Ok(file)
}
