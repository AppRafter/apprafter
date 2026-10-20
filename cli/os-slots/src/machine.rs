// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! This computer's machine id, its name, and whether it looks like a container or WSL
//! (D.4b B54, B76, B77; ADR 0069 §3; GOTCHA-328).
//!
//! The machine id's bytes are exactly what the vault hashes (B76): on Linux the 32 lowercase
//! hexadecimal characters of `/etc/machine-id` as ASCII, without the line end — a file that is
//! missing, empty, or anything but `^[0-9a-f]{32}\n?$` is no machine id (Debian's slim image has
//! none, Fedora's and WSL 1's are empty); on macOS the 16 raw bytes `gethostuuid(2)` fills, never
//! its string form. Windows has none here (the Hello slot's MachineGuid is d4b-w1's).
//!
//! A container or WSL may get a new id when it is re-created: [`Environment`] says when this
//! computer looks like one — `/run/.containerenv` (podman), `/.dockerenv` (docker), WSL's interop
//! entry under `/proc/sys/fs/binfmt_misc/` (`WSLInterop`, or `WSLInterop-late`), or `WSL_INTEROP`
//! set, which the caller reads: this crate reads no environment variable.

use std::fmt;
use std::io::{self, Read};
use std::path::Path;

use zeroize::Zeroizing;

use crate::HostFacts;

/// This computer's machine id, as the OS gave it (module docs). Zeroized when dropped; its
/// `Debug` shows only its length.
#[derive(Clone)]
pub struct MachineId(Zeroizing<Vec<u8>>);

impl MachineId {
    pub fn new(bytes: Vec<u8>) -> MachineId {
        MachineId(Zeroizing::new(bytes))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MachineId(<{} bytes>)", self.0.len())
    }
}

/// Why this computer has no machine id here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoMachineId {
    /// `/etc/machine-id` does not exist.
    Missing,
    /// It is empty (or holds only a line end).
    Empty,
    /// It holds something other than 32 lowercase hexadecimal characters (`uninitialized`,
    /// upper case, another length) or only `'0'`s, or `gethostuuid` returned zeros: a null id,
    /// which the vault refuses too (d4b-b1's `MachineId::new`).
    Malformed,
    /// It could not be read.
    Unreadable { kind: io::ErrorKind },
    /// This OS has no machine id for the account-password slot (Windows).
    NotOnThisOs,
}

/// Where the Linux machine id is, under the root a reader is given.
pub const LINUX_MACHINE_ID: &str = "etc/machine-id";

/// The machine id under `root` (`/` on a real system; a directory a test builds).
pub fn read_linux_machine_id(root: &Path) -> Result<MachineId, NoMachineId> {
    let file = match std::fs::File::open(root.join(LINUX_MACHINE_ID)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(NoMachineId::Missing),
        Err(e) => return Err(NoMachineId::Unreadable { kind: e.kind() }),
    };
    // 33 bytes is the longest valid file; one more says it is longer.
    let mut bytes = Zeroizing::new(Vec::with_capacity(34));
    if let Err(e) = file.take(34).read_to_end(&mut bytes) {
        return Err(NoMachineId::Unreadable { kind: e.kind() });
    }
    parse_linux_machine_id(&bytes)
}

/// `^[0-9a-f]{32}\n?$`, as the 32 characters' ASCII bytes. All `'0'`s is no machine id either:
/// it would bind every such slot to one constant, and the vault refuses it (d4b-b1), so it is
/// `no_machine_id` at plan time rather than a vault error after the check (critic round).
fn parse_linux_machine_id(bytes: &[u8]) -> Result<MachineId, NoMachineId> {
    let id = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if id.is_empty() {
        return Err(NoMachineId::Empty);
    }
    let hex = id.len() == 32 && id.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if hex && id.iter().any(|&b| b != b'0') {
        Ok(MachineId::new(id.to_vec()))
    } else {
        Err(NoMachineId::Malformed)
    }
}

/// Whether this computer looks like a container or WSL (module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Environment {
    pub container: bool,
    pub wsl: bool,
}

impl Environment {
    /// Its machine id may change when it is re-created: a slot bound to it then opens nothing
    /// here, and the recovery key opens the store (B76's `SlotNote::MachineIdMayChange`).
    pub fn machine_id_may_change(self) -> bool {
        self.container || self.wsl
    }
}

/// The marks under `root`, and whether the caller saw `WSL_INTEROP` set.
pub fn detect_environment(root: &Path, wsl_interop_set: bool) -> Environment {
    let present = |rel: &str| root.join(rel).symlink_metadata().is_ok();
    Environment {
        container: present("run/.containerenv") || present(".dockerenv"),
        wsl: wsl_interop_set
            || present("proc/sys/fs/binfmt_misc/WSLInterop")
            || present("proc/sys/fs/binfmt_misc/WSLInterop-late"),
    }
}

/// This computer's facts, read from `/` and the OS. Reading them asks nothing of the person and
/// authenticates nobody, so it needs no fence (unlike a check).
#[derive(Debug, Clone, Copy)]
pub struct SystemHost {
    wsl_interop_set: bool,
}

impl SystemHost {
    /// `wsl_interop_set`: whether the caller's environment has `WSL_INTEROP`.
    pub fn new(wsl_interop_set: bool) -> SystemHost {
        SystemHost { wsl_interop_set }
    }
}

impl HostFacts for SystemHost {
    fn machine_id(&self) -> Result<MachineId, NoMachineId> {
        system_machine_id()
    }

    fn environment(&self) -> Environment {
        if cfg!(target_os = "linux") {
            detect_environment(Path::new("/"), self.wsl_interop_set)
        } else {
            Environment::default()
        }
    }

    fn host_name(&self) -> Option<String> {
        host_name()
    }
}

#[cfg(target_os = "macos")]
fn system_machine_id() -> Result<MachineId, NoMachineId> {
    crate::macos::hardware_uuid()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn system_machine_id() -> Result<MachineId, NoMachineId> {
    read_linux_machine_id(Path::new("/"))
}

#[cfg(not(unix))]
fn system_machine_id() -> Result<MachineId, NoMachineId> {
    Err(NoMachineId::NotOnThisOs)
}

/// `gethostname(2)`, lossy; `None` when empty or unreadable.
#[cfg(unix)]
fn host_name() -> Option<String> {
    let mut buffer = [0_u8; 256];
    // SAFETY: `buffer` is writable for its length, and gethostname writes no more than that.
    let rc = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if rc != 0 {
        return None;
    }
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    let name = String::from_utf8_lossy(&buffer[..end]).into_owned();
    (!name.is_empty()).then_some(name)
}

#[cfg(not(unix))]
fn host_name() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

    /// A root whose `etc/machine-id` holds `content`, or has none.
    fn root_with(content: Option<&[u8]>) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        if let Some(bytes) = content {
            std::fs::create_dir_all(root.path().join("etc")).unwrap();
            std::fs::write(root.path().join(LINUX_MACHINE_ID), bytes).unwrap();
        }
        root
    }

    fn read(content: Option<&[u8]>) -> Result<MachineId, NoMachineId> {
        read_linux_machine_id(root_with(content).path())
    }

    #[test]
    fn a_machine_id_reads_alike_with_and_without_its_line_end() {
        for content in [&b"0123456789abcdef0123456789abcdef\n"[..], &ID[..]] {
            assert_eq!(read(Some(content)).unwrap().expose(), ID, "{content:?}");
        }
    }

    #[test]
    fn a_missing_or_empty_file_is_no_machine_id() {
        assert_eq!(read(None).err(), Some(NoMachineId::Missing));
        for content in [&b""[..], b"\n"] {
            assert_eq!(
                read(Some(content)).err(),
                Some(NoMachineId::Empty),
                "{content:?}"
            );
        }
    }

    #[test]
    fn upper_case_another_length_or_anything_else_is_malformed() {
        for content in [
            &b"0123456789ABCDEF0123456789abcdef\n"[..],
            b"0123456789abcdef0123456789abcde\n",
            b"0123456789abcdef0123456789abcdef0\n",
            b"0123456789abcdef0123456789abcdef\n\n",
            b"0123456789abcdef0123456789abcdef\r\n",
            b"0123456789abcdef0123456789abcdeg\n",
            b"uninitialized\n",
            b"00000000000000000000000000000000\n",
            &[b'a'; 200],
        ] {
            assert_eq!(
                read(Some(content)).err(),
                Some(NoMachineId::Malformed),
                "{content:?}"
            );
        }
    }

    #[test]
    fn the_machine_id_is_never_in_debug_output() {
        let id = MachineId::new(ID.to_vec());
        let text = format!("{id:?} {id:#?}");
        assert!(!text.contains("0123456789abcdef"), "{text}");
        assert!(text.contains("32 bytes"), "{text}");
    }

    #[test]
    fn a_container_or_wsl_is_seen_by_its_marks() {
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_environment(plain.path(), false),
            Environment::default()
        );
        assert!(!Environment::default().machine_id_may_change());
        for (mark, container, wsl) in [
            ("run/.containerenv", true, false),
            (".dockerenv", true, false),
            ("proc/sys/fs/binfmt_misc/WSLInterop", false, true),
            ("proc/sys/fs/binfmt_misc/WSLInterop-late", false, true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(mark);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
            let seen = detect_environment(root.path(), false);
            assert_eq!(seen, Environment { container, wsl }, "{mark}");
            assert!(seen.machine_id_may_change(), "{mark}");
        }
        let interop = detect_environment(plain.path(), true);
        assert_eq!(
            interop,
            Environment {
                container: false,
                wsl: true
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn this_host_has_a_name() {
        let name = SystemHost::new(false).host_name();
        assert!(name.as_deref().is_some_and(|n| !n.is_empty()), "{name:?}");
    }
}
