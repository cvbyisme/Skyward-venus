//! Venus FTS hardware wake gesture, through the installed Xiaomi touch ABI.
//! Mode 14 updates the controller's gesture worker. Its GET_CUR_VALUE cache
//! is not updated by this mode, and reading gesture_mask mutates the driver;
//! neither is a valid acknowledgement. Check both ioctl and the returned status.
use std::fs::OpenOptions;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

fn status(rc: i32, returned: i32) -> io::Result<()> {
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if rc != 0 || returned > 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "incomplete Xiaomi touch ioctl"));
    }
    if returned < 0 {
        return Err(io::Error::from_raw_os_error(-returned));
    }
    Ok(())
}

/// Arm the FTS double-tap interrupt. This does not keep the CPU awake.
pub fn set_double_tap(enabled: bool) -> io::Result<()> {
    // The input device is registered before the Xiaomi callbacks. With no
    // callback the ioctl leaves panel id 0 in the result and falsely looks
    // successful. This read-only FTS callback proves registration completed.
    let vendor = match std::fs::read_to_string("/sys/class/touch/touch_dev/touch_vendor") {
        Ok(value) => value,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(io::Error::from_raw_os_error(libc::EAGAIN)),
        Err(e) => return Err(e),
    };
    if vendor.is_empty() { return Err(io::Error::from_raw_os_error(libc::EAGAIN)); }
    if vendor.trim() != "1" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected Xiaomi touch vendor"));
    }
    let file = OpenOptions::new().read(true).write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW).open("/dev/xiaomi-touch")?;
    let mut values = [0i32; 256];
    values[1] = 14; // Touch_Doubletap_Mode; touch panel 0, SET_CUR_VALUE nr 0.
    values[2] = i32::from(enabled);
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), 0, values.as_mut_ptr()) };
    status(rc, values[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_successful_ioctl_does_not_hide_the_controller_error() {
        assert_eq!(status(0, -libc::EAGAIN).unwrap_err().raw_os_error(), Some(libc::EAGAIN));
        assert_eq!(status(0, -libc::ENODEV).unwrap_err().raw_os_error(), Some(libc::ENODEV));
        assert!(status(0, 0).is_ok());
        assert!(status(4, 0).is_err());
        assert!(status(0, 1).is_err());
    }
}
