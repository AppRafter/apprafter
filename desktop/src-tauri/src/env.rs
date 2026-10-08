// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the desktop reads from its process environment: an allow-list, nothing more (ADR 0067
//! §2).
//!
//! A desktop started from a terminal inherits that terminal's whole environment —
//! `HCLOUD_TOKEN`, `APPRAFTER_AGE_KEY`, the `APPRAFTER_HCLOUD_BASE_URL` a developer exported for
//! the CLI — and every cluster tab would then act on that one provider project. So the app reads
//! its environment only through an [`AllowListEnv`], which answers:
//!
//! - `APPRAFTER_CONFIG_DIR`: the target store root, the one the CLI opens too;
//! - `APPRAFTER_DESKTOP_DATA_DIR`: the desktop's own data directory, so a walk never touches the
//!   owner's settings and logs ([`data_dir_override`]), and never finds the owner's running
//!   instance ([`instance_identifier`]). It fails closed: set but empty or not Unicode, the app
//!   refuses to start rather than fall back on the owner's own files;
//! - in a test build only (cargo feature `test-build`), `APPRAFTER_HCLOUD_BASE_URL`, which the
//!   core then accepts only as a loopback `http://` URL ([`desktop_context`]);
//!
//! and `None` for every other name, however it is set. [`AllowListEnv::from_process`] is the one
//! place in `src/` that reads `std::env`: `tests/env_guard.rs` fails on any other.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use apprafter_core::context::HCLOUD_BASE_URL_ENV;
use apprafter_core::{Context, CoreResult, DesktopPolicy, EnvSource};
use cli_core::CONFIG_DIR_ENV;

/// Points the desktop's own files (settings, logs) at another directory.
pub const DATA_DIR_ENV: &str = "APPRAFTER_DESKTOP_DATA_DIR";

/// How an [`AllowListEnv`] reads a name it allows: the raw value, as the OS holds it.
type Lookup = dyn Fn(&str) -> Option<OsString> + Send + Sync;

/// The process environment, seen through the desktop's allow-list (see the module docs).
pub struct AllowListEnv {
    test_build: bool,
    lookup: Box<Lookup>,
}

impl AllowListEnv {
    /// The process environment. The app passes `cfg!(feature = "test-build")`.
    ///
    /// Through [`EnvSource::var`] a value that is not valid Unicode reads as unset, as
    /// [`EnvSource`] specifies (like `std::env::var(..).ok()`): the core takes every one of
    /// these values as a `String`. [`var_os`](Self::var_os) has the raw value, for the one
    /// name that must tell set-but-unreadable from unset ([`data_dir_override`]).
    pub fn from_process(test_build: bool) -> Self {
        Self::with_lookup(test_build, |key| std::env::var_os(key))
    }

    /// The same allow-list over `lookup` in place of the process environment, so a test never
    /// mutates the real one. `lookup` is asked only for a name the allow-list lets through.
    pub fn with_lookup(
        test_build: bool,
        lookup: impl Fn(&str) -> Option<OsString> + Send + Sync + 'static,
    ) -> Self {
        AllowListEnv {
            test_build,
            lookup: Box::new(lookup),
        }
    }

    /// Whether this is a test build's view: it then also answers `APPRAFTER_HCLOUD_BASE_URL`, and
    /// [`desktop_context`] builds under [`DesktopPolicy::TEST_BUILD`].
    pub fn test_build(&self) -> bool {
        self.test_build
    }

    /// The raw value of `key` when it is on the allow-list and set, Unicode or not; `None`
    /// for every other name.
    pub fn var_os(&self, key: &str) -> Option<OsString> {
        if self.allows(key) {
            (self.lookup)(key)
        } else {
            None
        }
    }

    /// Whether `key` is on this view's allow-list.
    pub fn allows(&self, key: &str) -> bool {
        key == CONFIG_DIR_ENV
            || key == DATA_DIR_ENV
            || (self.test_build && key == HCLOUD_BASE_URL_ENV)
    }

    fn policy(&self) -> DesktopPolicy {
        if self.test_build {
            DesktopPolicy::TEST_BUILD
        } else {
            DesktopPolicy::RELEASE
        }
    }
}

impl EnvSource for AllowListEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.var_os(key)?.into_string().ok()
    }
}

impl fmt::Debug for AllowListEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AllowListEnv")
            .field("test_build", &self.test_build)
            .finish_non_exhaustive()
    }
}

/// The core's [`Context`] for this process: the store root from `APPRAFTER_CONFIG_DIR` (else the
/// platform default the CLI uses too), and the real Hetzner API — or, in a test build, the
/// loopback mock `APPRAFTER_HCLOUD_BASE_URL` names; any other value there is refused with
/// [`CoreError::UnsafeOverride`](apprafter_core::CoreError::UnsafeOverride). Every CLI override
/// the process inherited (`HCLOUD_TOKEN` first) is ignored.
///
/// The policy follows `env`'s own build flag ([`AllowListEnv::test_build`]), so a release view
/// can never be paired with the test-build policy, nor the other way round.
pub fn desktop_context(env: &AllowListEnv) -> CoreResult<Context> {
    Context::from_desktop_env(env, env.policy())
}

/// Why the data-directory override cannot be used: the app refuses to start (exit code 2)
/// rather than run on the owner's own files.
#[derive(Debug, thiserror::Error)]
pub enum DataDirError {
    #[error("{DATA_DIR_ENV} is set but empty: unset it, or point it at a directory")]
    Empty,
    #[error("{DATA_DIR_ENV} is not valid Unicode ({0:?}): point it at a directory whose path is")]
    NotUnicode(OsString),
    #[error("{DATA_DIR_ENV} names {}, which cannot be used: {error}", dir.display())]
    Unusable { dir: PathBuf, error: io::Error },
}

/// Where the desktop keeps its own files in place of the platform's app-data directory:
/// `APPRAFTER_DESKTOP_DATA_DIR` when set; `Ok(None)` when unset. A walk points it at its
/// scratch directory, and [`prepare_data_dir`] makes it the one path every use agrees on.
///
/// It fails closed: set but empty, or set to a value that is not valid Unicode, is an error,
/// never "unset" — a walk whose variable broke must not run on the owner's settings and logs,
/// nor focus the owner's running app.
pub fn data_dir_override(env: &AllowListEnv) -> Result<Option<PathBuf>, DataDirError> {
    let Some(raw) = env.var_os(DATA_DIR_ENV) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Err(DataDirError::Empty);
    }
    let dir = raw.into_string().map_err(DataDirError::NotUnicode)?;
    Ok(Some(PathBuf::from(dir)))
}

/// The override `dir` as every use of it must see it: created when missing, then canonical —
/// absolute (a relative one against the working directory, as the CLI reads
/// `APPRAFTER_CONFIG_DIR`), with no `.`, `..`, trailing separator or symbolic link left. So
/// `x`, `x/`, `x/./` and `y/../x` are one directory, one instance ([`instance_identifier`])
/// and one set of app directories. On Windows the path keeps its usual `C:\…` form, not the
/// verbatim `\\?\` one `std::fs::canonicalize` gives (`dunce`, as Tauri resolves its own).
pub fn prepare_data_dir(dir: &Path) -> Result<PathBuf, DataDirError> {
    let unusable = |error| DataDirError::Unusable {
        dir: dir.to_path_buf(),
        error,
    };
    std::fs::create_dir_all(dir).map_err(unusable)?;
    dunce::canonicalize(dir).map_err(unusable)
}

/// The identifier the single-instance lock is keyed on: `base` itself, or, with a data-dir
/// override, `base.t<16 lowercase hex digits>` — so a walk's instance never finds (and focuses)
/// the owner's, nor two walks on different directories each other.
///
/// The digits are FNV-1a 64 over the directory as given — the app passes it through
/// [`prepare_data_dir`] first — as raw OS bytes: the bytes themselves on Unix, the UTF-16 code
/// units in little-endian order on Windows. FNV is fixed by its definition, unlike
/// `DefaultHasher`, so the identifier stays the same across runs and Rust releases. The `t`
/// keeps the new element a valid D-Bus name element (`[A-Za-z_][A-Za-z0-9_]*`, never a leading
/// digit): on Linux the single-instance plugin registers the identifier on the session bus.
pub fn instance_identifier(base: &str, data_dir: Option<&Path>) -> String {
    match data_dir {
        None => base.to_string(),
        Some(dir) => format!("{base}.t{:016x}", instance_hash(dir)),
    }
}

/// The webview's data store with a data-dir override, on macOS (see
/// [`window::build_main`](crate::window::build_main)): the 16 hex digits of
/// [`instance_identifier`]'s suffix, as bytes — so the store changes exactly when the instance
/// does, and stays the same across runs.
pub fn data_store_identifier(data_dir: &Path) -> [u8; 16] {
    let hash = instance_hash(data_dir);
    std::array::from_fn(|i| HEX_DIGITS[((hash >> (60 - 4 * i)) & 0xf) as usize])
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// FNV-1a 64 over `dir`'s raw OS bytes.
fn instance_hash(dir: &Path) -> u64 {
    fnv1a64(os_bytes(dir))
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a, 64 bits.
fn fnv1a64(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

/// A path's raw OS bytes: the bytes themselves on Unix.
#[cfg(unix)]
fn os_bytes(path: &Path) -> impl Iterator<Item = u8> + '_ {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().iter().copied()
}

/// A path's raw OS bytes: its UTF-16 code units, each little-endian, on Windows.
#[cfg(windows)]
fn os_bytes(path: &Path) -> impl Iterator<Item = u8> + '_ {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().flat_map(u16::to_le_bytes)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use apprafter_core::{CliOverrides, CoreError, MapEnv};

    use super::*;

    /// Everything a terminal might hand the app: the three names on the list and some it must
    /// never see.
    const AMBIENT: &[(&str, &str)] = &[
        ("APPRAFTER_CONFIG_DIR", "/tmp/store"),
        ("APPRAFTER_DESKTOP_DATA_DIR", "/tmp/walk"),
        ("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:9"),
        ("HCLOUD_TOKEN", "inherited-token"),
        ("APPRAFTER_AGE_KEY", "/tmp/age.key"),
        ("APPRAFTER_SSH_PRIVATE_KEY", "/tmp/id"),
        ("APPRAFTER_SERVER_TYPE", "cx99"),
        ("KUBECONFIG", "/tmp/kubeconfig"),
        ("HOME", "/home/someone"),
    ];

    fn env_of(test_build: bool, pairs: &[(&str, &str)]) -> AllowListEnv {
        let pairs: Vec<(&str, OsString)> = pairs.iter().map(|(k, v)| (*k, v.into())).collect();
        env_of_os(test_build, &pairs)
    }

    /// [`env_of`] with raw values, Unicode or not.
    fn env_of_os(test_build: bool, pairs: &[(&str, OsString)]) -> AllowListEnv {
        let map: BTreeMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        AllowListEnv::with_lookup(test_build, move |k| map.get(k).cloned())
    }

    #[test]
    fn a_release_build_answers_only_the_store_root_and_the_data_dir() {
        let env = env_of(false, AMBIENT);
        for (key, value) in AMBIENT {
            let expected = matches!(*key, "APPRAFTER_CONFIG_DIR" | "APPRAFTER_DESKTOP_DATA_DIR")
                .then(|| value.to_string());
            assert_eq!(env.var(key), expected, "{key}");
        }
        assert!(!env.test_build());
    }

    #[test]
    fn a_test_build_also_answers_the_api_base_and_nothing_else() {
        let env = env_of(true, AMBIENT);
        for (key, value) in AMBIENT {
            let expected = matches!(
                *key,
                "APPRAFTER_CONFIG_DIR" | "APPRAFTER_DESKTOP_DATA_DIR" | "APPRAFTER_HCLOUD_BASE_URL"
            )
            .then(|| value.to_string());
            assert_eq!(env.var(key), expected, "{key}");
        }
        assert!(env.test_build());
    }

    #[test]
    fn the_lookup_is_never_asked_for_a_name_off_the_list() {
        for test_build in [false, true] {
            let asked = Arc::new(Mutex::new(Vec::<String>::new()));
            let env = AllowListEnv::with_lookup(test_build, {
                let asked = asked.clone();
                move |k| {
                    asked.lock().unwrap().push(k.to_string());
                    Some("set".into())
                }
            });
            for (key, _) in AMBIENT {
                let _ = env.var(key);
            }
            let asked = asked.lock().unwrap().clone();
            assert!(
                asked.iter().all(|k| env.allows(k)),
                "test_build={test_build}: {asked:?}"
            );
            let mut expected = vec!["APPRAFTER_CONFIG_DIR", "APPRAFTER_DESKTOP_DATA_DIR"];
            if test_build {
                expected.push("APPRAFTER_HCLOUD_BASE_URL");
            }
            let mut asked_sorted = asked;
            asked_sorted.sort();
            assert_eq!(asked_sorted, expected, "test_build={test_build}");
        }
    }

    /// The real environment, read and never written: whatever this test process inherited.
    /// `PATH` is set in any process, so its `None` shows the list at work.
    #[test]
    fn from_process_reads_the_real_environment_through_the_list() {
        for test_build in [false, true] {
            let env = AllowListEnv::from_process(test_build);
            assert_eq!(env.test_build(), test_build);
            for key in [CONFIG_DIR_ENV, DATA_DIR_ENV] {
                assert_eq!(env.var(key), std::env::var(key).ok(), "{key}");
            }
            let base = test_build
                .then(|| std::env::var(HCLOUD_BASE_URL_ENV).ok())
                .flatten();
            assert_eq!(env.var(HCLOUD_BASE_URL_ENV), base);
            assert!(std::env::var_os("PATH").is_some());
            assert_eq!(env.var("PATH"), None);
            assert_eq!(env.var("HCLOUD_TOKEN"), None);
        }
    }

    #[test]
    fn a_name_on_the_list_reads_as_the_lookup_has_it() {
        let env = env_of(true, &[("APPRAFTER_CONFIG_DIR", "")]);
        assert_eq!(
            env.var("APPRAFTER_CONFIG_DIR").as_deref(),
            Some(""),
            "empty passes through: each consumer decides what empty means"
        );
        assert_eq!(env.var("APPRAFTER_DESKTOP_DATA_DIR"), None);
        assert_eq!(env.var("APPRAFTER_HCLOUD_BASE_URL"), None);
    }

    #[test]
    fn the_names_are_the_ones_the_core_and_the_cli_use() {
        assert_eq!(CONFIG_DIR_ENV, "APPRAFTER_CONFIG_DIR");
        assert_eq!(HCLOUD_BASE_URL_ENV, "APPRAFTER_HCLOUD_BASE_URL");
        assert_eq!(DATA_DIR_ENV, "APPRAFTER_DESKTOP_DATA_DIR");
    }

    /// The API base a context gets when nothing redirects it.
    fn real_api_base() -> String {
        let env = MapEnv::new().with("APPRAFTER_CONFIG_DIR", "/tmp/store");
        let base = Context::from_desktop_env(&env, DesktopPolicy::RELEASE)
            .unwrap()
            .hcloud_base_url()
            .to_string();
        assert!(base.starts_with("https://"), "{base}");
        base
    }

    /// WI-430's acceptance: an ambient `HCLOUD_TOKEN` is ignored, and so is any API base.
    #[test]
    fn a_release_context_ignores_the_inherited_token_and_any_api_base() {
        for base in ["http://evil.example", "http://127.0.0.1:9"] {
            let env = env_of(
                false,
                &[
                    ("APPRAFTER_CONFIG_DIR", "/tmp/store"),
                    ("HCLOUD_TOKEN", "inherited-token"),
                    ("APPRAFTER_HCLOUD_BASE_URL", base),
                ],
            );
            let ctx = desktop_context(&env).unwrap();
            assert_eq!(ctx.config_root(), Path::new("/tmp/store"));
            assert_eq!(ctx.overrides(), &CliOverrides::default());
            assert_eq!(ctx.hcloud_base_url(), real_api_base(), "{base}");
        }
    }

    #[test]
    fn a_test_build_context_honours_a_loopback_api_base_and_still_ignores_the_token() {
        let env = env_of(
            true,
            &[
                ("APPRAFTER_CONFIG_DIR", "/tmp/store"),
                ("HCLOUD_TOKEN", "inherited-token"),
                ("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:9"),
            ],
        );
        let ctx = desktop_context(&env).unwrap();
        assert_eq!(ctx.config_root(), Path::new("/tmp/store"));
        assert_eq!(ctx.hcloud_base_url(), "http://127.0.0.1:9");
        assert_eq!(ctx.overrides(), &CliOverrides::default());
    }

    #[test]
    fn a_test_build_context_refuses_a_non_loopback_api_base() {
        let env = env_of(
            true,
            &[
                ("APPRAFTER_CONFIG_DIR", "/tmp/store"),
                ("APPRAFTER_HCLOUD_BASE_URL", "http://evil.example"),
            ],
        );
        let err = desktop_context(&env).unwrap_err();
        assert!(
            matches!(err, CoreError::UnsafeOverride { var, .. } if var == HCLOUD_BASE_URL_ENV),
            "{err:?}"
        );
    }

    #[test]
    fn a_test_build_context_without_an_api_base_uses_the_real_one() {
        let env = env_of(true, &[("APPRAFTER_CONFIG_DIR", "/tmp/store")]);
        assert_eq!(
            desktop_context(&env).unwrap().hcloud_base_url(),
            real_api_base()
        );
    }

    #[test]
    fn the_data_dir_override_is_a_path_when_set_and_nothing_when_unset() {
        for test_build in [false, true] {
            let set = env_of(test_build, &[("APPRAFTER_DESKTOP_DATA_DIR", "/tmp/walk")]);
            assert_eq!(
                data_dir_override(&set).unwrap(),
                Some(PathBuf::from("/tmp/walk"))
            );
            assert_eq!(data_dir_override(&env_of(test_build, &[])).unwrap(), None);
        }
    }

    /// A broken variable must not read as unset: the walk would run on the owner's files.
    #[test]
    fn a_data_dir_override_set_but_empty_or_not_unicode_is_refused() {
        for test_build in [false, true] {
            let empty = env_of(test_build, &[("APPRAFTER_DESKTOP_DATA_DIR", "")]);
            let err = data_dir_override(&empty).unwrap_err();
            assert!(matches!(err, DataDirError::Empty), "{err:?}");
            assert_eq!(
                err.to_string(),
                "APPRAFTER_DESKTOP_DATA_DIR is set but empty: unset it, or point it at a directory"
            );
            // Bytes that are no UTF-8 on Unix, an unpaired surrogate on Windows.
            #[cfg(unix)]
            let raw = {
                use std::os::unix::ffi::OsStringExt;
                OsString::from_vec(b"/tmp/walk-\xff".to_vec())
            };
            #[cfg(windows)]
            let raw = {
                use std::os::windows::ffi::OsStringExt;
                OsString::from_wide(&[u16::from(b'w'), 0xd800])
            };
            let env = env_of_os(test_build, &[("APPRAFTER_DESKTOP_DATA_DIR", raw.clone())]);
            assert_eq!(env.var(DATA_DIR_ENV), None, "unset, read as a String");
            let err = data_dir_override(&env).unwrap_err();
            assert!(
                matches!(&err, DataDirError::NotUnicode(got) if *got == raw),
                "{err:?}"
            );
            assert!(err.to_string().contains("not valid Unicode"), "{err}");
        }
    }

    #[test]
    fn a_prepared_data_dir_is_one_path_however_it_is_spelled() {
        let root = tempfile::tempdir().unwrap();
        // Canonical already, so the comparison below is not thrown by a symlinked temp dir.
        let root = dunce::canonicalize(root.path()).unwrap();
        std::fs::create_dir(root.join("y")).unwrap();
        let x = root.join("x");
        let spellings = [
            x.clone(),
            PathBuf::from(format!("{}/", x.display())),
            x.join("."),
            root.join("y").join("..").join("x"),
        ];
        for spelling in &spellings {
            let prepared = prepare_data_dir(spelling).unwrap();
            assert_eq!(prepared, x, "{spelling:?}");
            assert!(prepared.is_dir(), "{spelling:?}: created");
            assert_eq!(
                instance_identifier(BASE, Some(&prepared)),
                instance_identifier(BASE, Some(&x)),
                "{spelling:?}"
            );
            assert_eq!(
                data_store_identifier(&prepared),
                data_store_identifier(&x),
                "{spelling:?}"
            );
        }
        // Different spellings, hashed as given, would have been different instances.
        assert_ne!(
            instance_identifier(BASE, Some(&spellings[0])),
            instance_identifier(BASE, Some(&spellings[1]))
        );
    }

    #[test]
    fn a_data_dir_that_cannot_be_created_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("a-file");
        std::fs::write(&file, b"").unwrap();
        let err = prepare_data_dir(&file.join("walk")).unwrap_err();
        assert!(matches!(err, DataDirError::Unusable { .. }), "{err:?}");
        assert!(err.to_string().contains("cannot be used"), "{err}");
    }

    #[test]
    fn the_data_store_is_the_identifiers_hex_digits_as_bytes() {
        let dir = Path::new("/tmp/walk");
        let id = instance_identifier(BASE, Some(dir));
        let digits = id.rsplit_once(".t").unwrap().1;
        assert_eq!(&data_store_identifier(dir), digits.as_bytes());
        assert_ne!(
            data_store_identifier(dir),
            data_store_identifier(Path::new("/tmp/walk2"))
        );
    }

    const BASE: &str = "dev.apprafter.desktop";

    /// Paths that differ in a trailing slash, a case, a byte, relativity — and one whose hash
    /// starts with a zero digit on both Unix and Windows, so the padding is exercised.
    const PATHS: &[&str] = &[
        "/tmp/walk",
        "/tmp/walk/",
        "/tmp/Walk",
        "/tmp/walk2",
        "walk",
        "/tmp/walk-60",
        "",
    ];

    #[test]
    fn without_an_override_the_identifier_is_the_base() {
        assert_eq!(instance_identifier(BASE, None), BASE);
    }

    /// Pinned: a changed hash would let a walk started by an older build and one started by a
    /// newer build both run, and is caught here first.
    #[test]
    fn the_identifier_suffix_is_pinned() {
        let expected = if cfg!(windows) {
            "dev.apprafter.desktop.t3776a7a6fa698a4d"
        } else {
            "dev.apprafter.desktop.t8c5108fba7cbf86d"
        };
        assert_eq!(
            instance_identifier(BASE, Some(Path::new("/tmp/walk"))),
            expected
        );
    }

    #[test]
    fn fnv1a64_matches_the_published_vectors() {
        assert_eq!(fnv1a64(*b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(*b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(*b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn the_identifier_is_deterministic_distinct_and_a_valid_dbus_name() {
        let ids: Vec<String> = PATHS
            .iter()
            .map(|p| instance_identifier(BASE, Some(Path::new(p))))
            .collect();
        for (p, id) in PATHS.iter().zip(&ids) {
            assert_eq!(&instance_identifier(BASE, Some(Path::new(p))), id);
            let suffix = id
                .strip_prefix("dev.apprafter.desktop.t")
                .unwrap_or_else(|| panic!("{p:?}: {id}"));
            assert_eq!(suffix.len(), 16, "{p:?}: {id}");
            assert!(
                suffix
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{p:?}: {id}"
            );
            for element in id.split('.') {
                assert!(is_dbus_name_element(element), "{p:?}: {element:?} in {id}");
            }
        }
        let mut distinct = ids.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), ids.len(), "{ids:?}");
        assert!(ids.iter().any(|id| id.contains(".t0")), "{ids:?}");
    }

    fn is_dbus_name_element(s: &str) -> bool {
        let mut bytes = s.bytes();
        bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }
}
