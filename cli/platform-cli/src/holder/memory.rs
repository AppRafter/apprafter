// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The page a holder keeps the data key in (D.4a T54): one page of its own, locked against
//! paging where the limits allow (`mlock`; `VirtualLock`), kept out of core dumps on Linux
//! (`MADV_DONTDUMP`), and zeroed before it is unmapped. A hibernation image still holds it (O4;
//! `lock status` says so).

use std::fmt;
use std::io;

/// One page holding a 32-byte key.
pub struct LockedKey {
    page: *mut u8,
    len: usize,
    locked: bool,
}

// SAFETY: the page is owned by this value alone; `&self` hands out only shared reads.
unsafe impl Send for LockedKey {}
// SAFETY: as above.
unsafe impl Sync for LockedKey {}

impl LockedKey {
    pub fn new() -> io::Result<LockedKey> {
        os_new()
    }

    /// Whether the page is locked against paging (reported as `memory_locked`).
    pub fn locked(&self) -> bool {
        self.locked
    }

    pub fn bytes(&self) -> &[u8; 32] {
        // SAFETY: the page is at least 32 bytes, page-aligned, and lives as long as `self`.
        unsafe { &*(self.page as *const [u8; 32]) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8; 32] {
        // SAFETY: as above, and `&mut self` is the only access.
        unsafe { &mut *(self.page as *mut [u8; 32]) }
    }
}

impl Drop for LockedKey {
    fn drop(&mut self) {
        // SAFETY: the page is at least 32 bytes and still mapped.
        unsafe { wipe(self.page, 32) };
        os_free(self);
    }
}

impl fmt::Debug for LockedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LockedKey(..)")
    }
}

/// Zero `n` bytes at `p` with writes the compiler may not drop.
///
/// # Safety
/// `p` must be valid for `n` byte writes.
pub(crate) unsafe fn wipe(p: *mut u8, n: usize) {
    for i in 0..n {
        // SAFETY: the caller's contract.
        unsafe { std::ptr::write_volatile(p.add(i), 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

#[cfg(unix)]
fn os_new() -> io::Result<LockedKey> {
    // SAFETY: sysconf has no preconditions.
    let len = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
        .ok()
        .filter(|n| *n >= 32)
        .unwrap_or(4096);
    // SAFETY: an anonymous private mapping of `len` bytes; checked below.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_os = "linux")]
    // SAFETY: `p` is the mapping above; a failure only leaves the page in core dumps, which
    // RLIMIT_CORE 0 and PR_SET_DUMPABLE 0 also keep out.
    unsafe {
        libc::madvise(p, len, libc::MADV_DONTDUMP)
    };
    // SAFETY: as above.
    let locked = unsafe { libc::mlock(p, len) } == 0;
    Ok(LockedKey {
        page: p.cast(),
        len,
        locked,
    })
}

#[cfg(unix)]
fn os_free(k: &mut LockedKey) {
    // SAFETY: the mapping `os_new` made, unlocked if it was locked, then unmapped once.
    unsafe {
        if k.locked {
            libc::munlock(k.page.cast(), k.len);
        }
        libc::munmap(k.page.cast(), k.len);
    }
}

#[cfg(windows)]
fn os_new() -> io::Result<LockedKey> {
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualLock, MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE,
    };
    let len = 4096;
    // SAFETY: a fresh committed page; checked below.
    let p = unsafe {
        VirtualAlloc(
            std::ptr::null(),
            len,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };
    if p.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the page above.
    let locked = unsafe { VirtualLock(p, len) } != 0;
    Ok(LockedKey {
        page: p.cast(),
        len,
        locked,
    })
}

#[cfg(windows)]
fn os_free(k: &mut LockedKey) {
    use windows_sys::Win32::System::Memory::{VirtualFree, VirtualUnlock, MEM_RELEASE};
    // SAFETY: the page `os_new` allocated, unlocked if it was locked, then released once.
    unsafe {
        if k.locked {
            VirtualUnlock(k.page.cast(), k.len);
        }
        VirtualFree(k.page.cast(), 0, MEM_RELEASE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T54: the key's page is locked against paging (one page fits every default
    /// `RLIMIT_MEMLOCK`: 64 KiB on older kernels, 8 MiB since 5.16), holds what is written,
    /// and never shows it in `Debug`.
    #[test]
    fn a_key_page_is_locked_and_keeps_its_key_to_itself() {
        let mut k = LockedKey::new().unwrap();
        assert!(k.locked(), "one page must lock under the default limits");
        k.bytes_mut().copy_from_slice(&[0x5a; 32]);
        assert_eq!(k.bytes(), &[0x5a; 32]);
        assert_eq!(format!("{k:?}"), "LockedKey(..)");
    }

    #[test]
    fn wipe_zeroes_what_it_is_given() {
        let mut buf = [0xa5u8; 48];
        // SAFETY: `buf` is 48 bytes of this test's stack.
        unsafe { wipe(buf.as_mut_ptr(), 32) };
        assert_eq!(&buf[..32], &[0u8; 32]);
        assert_eq!(&buf[32..], &[0xa5u8; 16], "only what it was told");
    }
}
