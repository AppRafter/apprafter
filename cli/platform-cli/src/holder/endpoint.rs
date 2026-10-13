// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Where a terminal's holder listens (ADR 0068 §4; D.4a T49, T50, holder §2).
//!
//! The id is the first 10 bytes of SHA-256 over a domain tag and the binding — Unix: the user,
//! the store's id and canonical root, and the session; Windows: the user's SID, the store's id
//! and root — as 16 lowercase base32 characters. A store copied elsewhere, another store and
//! another terminal never share an endpoint, and a scratch store can never name an owner's. A
//! Unix socket is `<base>/h-<id>.sock` beside its `h-<id>.lock`; a Windows pipe also carries the
//! holder's pid, so two consoles may hold one store.

use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::protocol::{path_bytes, HolderUnavailable, StoreBinding};

const DOMAIN: &[u8] = b"apprafter-holder-v1";

/// `sun_path`, its terminating NUL included (macOS `sys/un.h`; Linux `unix(7)`).
#[cfg(target_os = "macos")]
pub const SUN_PATH_MAX: usize = 104;
#[cfg(not(target_os = "macos"))]
pub const SUN_PATH_MAX: usize = 108;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EndpointId(pub(crate) [u8; 10]);

fn length_prefixed(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u32).to_le_bytes());
    h.update(bytes);
}

impl EndpointId {
    /// Unix: the user, the store, and the session the holder binds.
    pub fn for_session(uid: u32, store: &StoreBinding, sid: i32) -> EndpointId {
        let mut h = Sha256::new();
        h.update(DOMAIN);
        h.update([0u8]);
        h.update(uid.to_le_bytes());
        length_prefixed(&mut h, store.id.as_bytes());
        length_prefixed(&mut h, &path_bytes(&store.root));
        h.update(sid.to_le_bytes());
        EndpointId::of(h)
    }

    /// Windows: the user's SID and the store.
    pub fn for_user(user_sid: &str, store: &StoreBinding) -> EndpointId {
        let mut h = Sha256::new();
        h.update(DOMAIN);
        h.update([1u8]);
        length_prefixed(&mut h, user_sid.as_bytes());
        length_prefixed(&mut h, store.id.as_bytes());
        length_prefixed(&mut h, &path_bytes(&store.root));
        EndpointId::of(h)
    }

    fn of(h: Sha256) -> EndpointId {
        let digest = h.finalize();
        let mut id = [0u8; 10];
        id.copy_from_slice(&digest[..10]);
        EndpointId(id)
    }
}

impl fmt::Display for EndpointId {
    /// RFC 4648 base32, lowercase: 80 bits, 16 characters, no padding.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
        let bits = self
            .0
            .iter()
            .fold(0u128, |acc, b| (acc << 8) | u128::from(*b));
        for i in (0..16).rev() {
            f.write_char(char::from(ALPHABET[((bits >> (i * 5)) & 31) as usize]))?;
        }
        Ok(())
    }
}

impl fmt::Debug for EndpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EndpointId({self})")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// Unix: the socket, and the lock file whose `flock` keeps one holder per id.
    Socket { path: PathBuf, lock: PathBuf },
    /// Windows: the pipe is `pipe_name(id, <holder pid>)`.
    Pipe { id: EndpointId },
}

/// Unix: `<base>/h-<id>.sock` and `.lock`, refused when the socket path with its NUL does not
/// fit `sun_path`.
pub fn socket_endpoint(base: &Path, id: &EndpointId) -> Result<Endpoint, HolderUnavailable> {
    let path = base.join(format!("h-{id}.sock"));
    let len = path_bytes(&path).len() + 1;
    if len > SUN_PATH_MAX {
        return Err(HolderUnavailable::PathTooLong {
            len,
            max: SUN_PATH_MAX,
        });
    }
    Ok(Endpoint::Socket {
        lock: base.join(format!("h-{id}.lock")),
        path,
    })
}

/// Windows: the holder's pipe. The one place a pipe prefix is spelled (G2, `ENDPOINT_LITERALS`).
pub fn pipe_name(id: &EndpointId, pid: u32) -> String {
    format!(r"\\.\pipe\apprafter-holder-{id}-{pid}")
}

/// What a base check judges: an `lstat` of the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirFacts {
    pub is_dir: bool,
    pub is_symlink: bool,
    pub uid: u32,
    pub mode: u32,
}

/// T49: a directory, no symbolic link, owned by `euid`, open to no one else.
pub fn judge(facts: DirFacts, euid: u32) -> Result<(), String> {
    if facts.is_symlink {
        return Err("it is a symbolic link".into());
    }
    if !facts.is_dir {
        return Err("it is not a directory".into());
    }
    if facts.uid != euid {
        return Err(format!("it belongs to uid {}, not {euid}", facts.uid));
    }
    if facts.mode & 0o077 != 0 {
        return Err(format!("its mode {:o} lets others in", facts.mode & 0o777));
    }
    Ok(())
}

/// Unix: make `dir` (0700) when it is missing — its parent must exist — then judge it. The
/// holder runs it before `bind`, a client before `connect`.
#[cfg(unix)]
pub fn check_base(dir: &Path) -> Result<(), HolderUnavailable> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let refuse = |reason: String| HolderUnavailable::UnsafeRuntimeDir {
        path: dir.to_path_buf(),
        reason,
    };
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(refuse(e.to_string())),
    }
    let meta = std::fs::symlink_metadata(dir).map_err(|e| refuse(e.to_string()))?;
    let facts = DirFacts {
        is_dir: meta.is_dir(),
        is_symlink: meta.file_type().is_symlink(),
        uid: meta.uid(),
        mode: meta.mode(),
    };
    // SAFETY: geteuid has no failure mode.
    judge(facts, unsafe { libc::geteuid() }).map_err(refuse)
}

/// Unix: this process's session's endpoint for `store` under `base` (checked first).
#[cfg(unix)]
pub fn session_endpoint(base: &Path, store: &StoreBinding) -> Result<Endpoint, HolderUnavailable> {
    check_base(base)?;
    // SAFETY: neither call can fail for this process.
    let (uid, sid) = unsafe { (libc::geteuid(), libc::getsid(0)) };
    socket_endpoint(base, &EndpointId::for_session(uid, store, sid))
}

/// Unix: this terminal's endpoint for `store`, under the context's holder base (T49).
#[cfg(unix)]
pub fn endpoint_for(
    ctx: &apprafter_core::Context,
    store: &StoreBinding,
) -> Result<Endpoint, HolderUnavailable> {
    let base = ctx.holder_base().ok_or(HolderUnavailable::NoRuntimeDir)?;
    session_endpoint(Path::new(&base.dir), store)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(root: &str) -> StoreBinding {
        StoreBinding {
            id: "0123456789abcdef".into(),
            root: PathBuf::from(root),
        }
    }

    /// T50: the Unix id is frozen (`$E/goldens.py`), and the session, the store's id and its
    /// root each change it — a copied store and another terminal never share an endpoint.
    #[cfg(unix)]
    #[test]
    fn the_unix_endpoint_id_is_golden_and_keyed_by_session_store_and_root() {
        let s = store("/home/a/.config/apprafter");
        assert_eq!(
            EndpointId::for_session(1000, &s, 4242).to_string(),
            "hrke272ahrrrc6ux"
        );
        assert_eq!(
            EndpointId::for_session(1000, &s, 4243).to_string(),
            "ek7qwr6c7s5vs25e"
        );
        let mut other_id = s.clone();
        other_id.id = "fedcba9876543210".into();
        let ids = [
            EndpointId::for_session(1001, &s, 4242),
            EndpointId::for_session(1000, &other_id, 4242),
            EndpointId::for_session(1000, &store("/home/a/copy"), 4242),
        ];
        for id in ids {
            assert_ne!(id.to_string(), "hrke272ahrrrc6ux");
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_endpoint_id_is_golden() {
        let s = store(r"C:\Users\a\AppData\Roaming\apprafter");
        assert_eq!(
            EndpointId::for_user("S-1-5-21-1-2-3-1001", &s).to_string(),
            "z4id52ubzohuhg45"
        );
    }

    /// T50: the socket path with its NUL fits `sun_path` (104 on macOS, 108 on Linux) — the
    /// worst realistic macOS per-user temporary directory included — and one byte more is
    /// refused by name, before any `bind`. Unix only: `sun_path` is a Unix limit, and on
    /// Windows `path_bytes` is UTF-16 (two bytes a character), where no socket is ever bound.
    #[cfg(unix)]
    #[test]
    fn a_socket_path_fits_sun_path_or_is_refused_by_name() {
        let id = EndpointId([0; 10]);
        let mac = Path::new("/var/folders/zz/zyxvpxvq6csfxvn_n0000000000000/T/apprafter");
        assert!(matches!(
            socket_endpoint(mac, &id),
            Ok(Endpoint::Socket { .. })
        ));
        // "/" + n × "a" + "/h-" + 16 + ".sock" + NUL = n + 26 bytes.
        let fits = format!("/{}", "a".repeat(SUN_PATH_MAX - 26));
        assert!(socket_endpoint(Path::new(&fits), &id).is_ok());
        let over = format!("/{}", "a".repeat(SUN_PATH_MAX - 25));
        assert_eq!(
            socket_endpoint(Path::new(&over), &id),
            Err(HolderUnavailable::PathTooLong {
                len: SUN_PATH_MAX + 1,
                max: SUN_PATH_MAX
            })
        );
    }

    #[test]
    fn the_socket_and_its_lock_sit_side_by_side_and_the_pipe_names_its_holder() {
        let id = EndpointId([0xff; 10]);
        assert_eq!(id.to_string(), "7777777777777777");
        match socket_endpoint(Path::new("/r/apprafter"), &id).unwrap() {
            Endpoint::Socket { path, lock } => {
                assert_eq!(path, Path::new("/r/apprafter/h-7777777777777777.sock"));
                assert_eq!(lock, Path::new("/r/apprafter/h-7777777777777777.lock"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            pipe_name(&id, 4242),
            r"\\.\pipe\apprafter-holder-7777777777777777-4242"
        );
    }

    /// T49: a base is a directory of this user's, open to no one else, and no symbolic link.
    #[test]
    fn a_base_is_a_private_directory_of_this_user() {
        let ok = DirFacts {
            is_dir: true,
            is_symlink: false,
            uid: 1000,
            mode: 0o40700,
        };
        assert_eq!(judge(ok, 1000), Ok(()));
        for (why, facts) in [
            (
                "a symlink",
                DirFacts {
                    is_symlink: true,
                    ..ok
                },
            ),
            (
                "a file",
                DirFacts {
                    is_dir: false,
                    ..ok
                },
            ),
            ("another owner", DirFacts { uid: 0, ..ok }),
            (
                "group-readable",
                DirFacts {
                    mode: 0o40740,
                    ..ok
                },
            ),
            (
                "world-searchable",
                DirFacts {
                    mode: 0o40701,
                    ..ok
                },
            ),
        ] {
            assert!(judge(facts, 1000).is_err(), "{why}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn check_base_makes_a_private_directory_and_refuses_an_open_one() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new_in("/tmp").unwrap();
        let base = tmp.path().join("apprafter");
        check_base(&base).unwrap();
        let mode = std::fs::metadata(&base).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            check_base(&base),
            Err(HolderUnavailable::UnsafeRuntimeDir { .. })
        ));
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(tmp.path(), &link).unwrap();
        assert!(matches!(
            check_base(&link),
            Err(HolderUnavailable::UnsafeRuntimeDir { .. })
        ));
    }
}
