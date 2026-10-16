// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! macOS's own facts: the hardware UUID, and the per-user temporary directory the back-off's
//! record lives in. Both are read without privileges and follow no environment variable
//! (GOTCHA-328).

use std::io;

use zeroize::Zeroize;

use crate::machine::{MachineId, NoMachineId};

/// `gethostuuid(2)`: the hardware UUID's 16 raw bytes (B76). It survives an OS reinstall on the
/// same Mac and differs on another.
pub(crate) fn hardware_uuid() -> Result<MachineId, NoMachineId> {
    let mut id = [0_u8; 16];
    let wait = libc::timespec {
        tv_sec: 5,
        tv_nsec: 0,
    };
    // SAFETY: `id` is the 16 writable bytes gethostuuid(2) fills; `wait` lives across the call.
    let rc = unsafe { libc::gethostuuid(id.as_mut_ptr(), &wait) };
    if rc != 0 {
        return Err(NoMachineId::Unreadable {
            kind: io::Error::last_os_error().kind(),
        });
    }
    let out = if id == [0; 16] {
        Err(NoMachineId::Malformed)
    } else {
        Ok(MachineId::new(id.to_vec()))
    };
    id.zeroize();
    out
}

/// `confstr(_CS_DARWIN_USER_TEMP_DIR)`: this account's temporary directory, which follows
/// neither `HOME` nor `TMPDIR` (GOTCHA-328). The back-off's record lives there.
pub(crate) fn darwin_user_temp_dir() -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buffer = vec![0_u8; 1024];
    // SAFETY: `buffer` is writable for its length; confstr writes at most that many bytes, NUL
    // included, and returns the length it needs (0 on an error).
    let needed = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if needed == 0 || needed > buffer.len() {
        return None;
    }
    // Absolute only, as cli-core's twin (`paths::darwin_user_temp_dir`; critic round).
    let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&buffer[..needed - 1]));
    path.is_absolute().then_some(path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_hardware_uuid_is_16_bytes_and_the_same_each_time() {
        let first = super::hardware_uuid().unwrap();
        let again = super::hardware_uuid().unwrap();
        assert_eq!(first.expose().len(), 16);
        assert_eq!(first.expose(), again.expose());
    }
}
