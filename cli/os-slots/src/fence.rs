// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The fence (D.4b B16, B82; ADR 0069 §9; CLAUDE.md, "Never check an account password or ask
//! Windows Hello on the user's computers"): [`admit`] is the only way to a real
//! account-password backend.
//!
//! With `APPRAFTER_SANDBOX`'s roots — the CLI and the desktop pass them, in every build, and
//! `None` when the variable is unset — a real backend is made only for an account whose passwd
//! home (`getpwuid_r`, never `$HOME`) lies strictly inside a root and holds the marker file
//! [`TEST_ACCOUNT_MARKER`]: a regular file, not a link, the account owns. The agent's VMs and
//! containers and the CI jobs make it for the test accounts they create. A person's home is
//! outside every scratch root and has no marker, so an agent's run is refused before PAM; a root
//! of `/home` or `/` does not admit a home that is not marked. The roots only add refusals:
//! without them nothing is fenced (the person's own use, and the owner's walk).
//!
//! `apprafter-core/tests/guards.rs` holds the rest: no test names a real backend except the
//! `#[ignore]`d real cases, which pass `Some` roots and a marked account (G-OS1), and nothing but
//! [`admit`] builds one (G-OS5). d4b-w1 adds the Windows Hello backend beside it, under the same
//! rule (every Hello call refused under the roots).

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use zeroize::Zeroizing;

use crate::account::{self, Account};
use crate::check::{Checker, RecordAt};
use crate::outcome::{BackoffState, Os, PasswordCheck, PasswordStatus, Unavailable};
use crate::pam::Pam;
use crate::AccountPassword;

/// The file that marks a test account's home (B16).
pub const TEST_ACCOUNT_MARKER: &str = ".apprafter-test-account";

/// A real backend the fence refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fenced {
    /// The account it would have checked (`(none)` without one).
    pub account: String,
    pub why: FencedWhy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FencedWhy {
    /// The running uid has no passwd entry.
    NoAccount,
    /// A root that is not an existing directory given by an absolute path.
    RootUnusable { root: PathBuf },
    /// The passwd home is not strictly inside any root, or cannot be resolved.
    HomeOutsideRoots { home: PathBuf },
    /// The home holds no marker the account owns.
    NoMarker { home: PathBuf },
}

impl fmt::Display for Fenced {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let who = &self.account;
        match &self.why {
            FencedWhy::NoAccount => {
                write!(f, "APPRAFTER_SANDBOX is set and this process has no account to check")
            }
            FencedWhy::RootUnusable { root } => write!(
                f,
                "APPRAFTER_SANDBOX names {}, which is not an existing directory given by an absolute path",
                root.display()
            ),
            FencedWhy::HomeOutsideRoots { home } => write!(
                f,
                "the account password of {who} is not checked under APPRAFTER_SANDBOX: its home {} is not inside the sandbox",
                home.display()
            ),
            FencedWhy::NoMarker { home } => write!(
                f,
                "the account password of {who} is not checked under APPRAFTER_SANDBOX: its home {} holds no {TEST_ACCOUNT_MARKER} of its own",
                home.display()
            ),
        }
    }
}

impl std::error::Error for Fenced {}

/// The real account-password backend. Only [`admit`] makes one.
pub struct SystemAccountPassword {
    inner: Checker,
}

impl fmt::Debug for SystemAccountPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SystemAccountPassword")
            .field("os", &self.inner.os)
            .field("account", &self.inner.account)
            .finish_non_exhaustive()
    }
}

/// The real account-password backend for the running account, unless the fence refuses it
/// (module docs). `sandbox_roots`: `APPRAFTER_SANDBOX`'s roots, `None` when it is unset.
pub fn admit(sandbox_roots: Option<&[PathBuf]>) -> Result<SystemAccountPassword, Fenced> {
    let account = account::current_account();
    decide(sandbox_roots, account.as_ref())?;
    let os = Os::this();
    Ok(SystemAccountPassword {
        inner: Checker {
            os,
            account,
            pam: Pam::system(os),
            record_at: RecordAt::System,
        },
    })
}

/// The fence's rule (module docs), without making anything.
pub(crate) fn decide(
    sandbox_roots: Option<&[PathBuf]>,
    account: Option<&Account>,
) -> Result<(), Fenced> {
    let Some(roots) = sandbox_roots else {
        return Ok(());
    };
    let Some(account) = account else {
        return Err(Fenced {
            account: "(none)".to_owned(),
            why: FencedWhy::NoAccount,
        });
    };
    let fenced = |why| Fenced {
        account: account.name_lossy(),
        why,
    };
    let mut inside = Vec::with_capacity(roots.len());
    for root in roots {
        match root.canonicalize() {
            Ok(canonical) if root.is_absolute() && canonical.is_dir() => inside.push(canonical),
            _ => return Err(fenced(FencedWhy::RootUnusable { root: root.clone() })),
        }
    }
    let Ok(home) = account.home.canonicalize() else {
        return Err(fenced(FencedWhy::HomeOutsideRoots {
            home: account.home.clone(),
        }));
    };
    if !inside
        .iter()
        .any(|root| home != *root && home.starts_with(root))
    {
        return Err(fenced(FencedWhy::HomeOutsideRoots { home }));
    }
    if !marked(&home, account.uid) {
        return Err(fenced(FencedWhy::NoMarker { home }));
    }
    Ok(())
}

/// The marker: a regular file (not a link) in `home` that `uid` owns.
#[cfg(unix)]
fn marked(home: &Path, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(home.join(TEST_ACCOUNT_MARKER))
        .is_ok_and(|m| m.is_file() && m.uid() == uid)
}

/// No test accounts here (Windows has no account-password slot).
#[cfg(not(unix))]
fn marked(_home: &Path, _uid: u32) -> bool {
    false
}

impl AccountPassword for SystemAccountPassword {
    fn os(&self) -> Os {
        self.inner.os
    }

    fn account_name(&self) -> Option<String> {
        self.inner.account.as_ref().map(Account::name_lossy)
    }

    fn backend(&self) -> Result<(), Unavailable> {
        self.inner.backend()
    }

    /// The system clock, never the caller's `now`: no caller moves the back-off (B12; critic
    /// round).
    fn backoff(&self, _now: SystemTime) -> Result<BackoffState, Unavailable> {
        self.inner.backoff(SystemTime::now())
    }

    fn password_status(&self) -> PasswordStatus {
        self.inner.password_status()
    }

    /// The system clock, as `backoff`.
    fn check(&self, password: &Zeroizing<String>, _now: SystemTime) -> PasswordCheck {
        self.inner.check(password, SystemTime::now())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn account(home: &Path) -> Account {
        // SAFETY: getuid cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        Account {
            name: b"walker".to_vec(),
            uid,
            home: home.to_path_buf(),
        }
    }

    /// A sandbox root with `walker`'s home inside it, marked or not.
    fn sandbox(marked: bool) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home").join("walker");
        std::fs::create_dir_all(&home).unwrap();
        if marked {
            std::fs::write(home.join(TEST_ACCOUNT_MARKER), b"").unwrap();
        }
        (root, home)
    }

    fn roots(dirs: &[&Path]) -> Vec<PathBuf> {
        dirs.iter().map(|dir| dir.to_path_buf()).collect()
    }

    #[test]
    fn without_sandbox_roots_nothing_is_fenced() {
        assert_eq!(decide(None, None), Ok(()));
        assert_eq!(
            decide(None, Some(&account(Path::new("/nonexistent")))),
            Ok(())
        );
    }

    #[test]
    fn a_marked_home_strictly_inside_a_root_is_admitted() {
        let (root, home) = sandbox(true);
        assert_eq!(
            decide(Some(&roots(&[root.path()])), Some(&account(&home))),
            Ok(())
        );
        let parent = home.parent().unwrap();
        assert_eq!(
            decide(Some(&roots(&[parent])), Some(&account(&home))),
            Ok(())
        );
    }

    #[test]
    fn an_account_without_the_marker_is_fenced() {
        let (root, home) = sandbox(false);
        let refused = decide(Some(&roots(&[root.path()])), Some(&account(&home))).unwrap_err();
        assert_eq!(refused.account, "walker");
        let canonical = home.canonicalize().unwrap();
        assert_eq!(refused.why, FencedWhy::NoMarker { home: canonical });
    }

    #[test]
    fn a_home_that_is_a_root_is_not_strictly_inside() {
        let (_root, home) = sandbox(true);
        let refused = decide(Some(&roots(&[home.as_path()])), Some(&account(&home))).unwrap_err();
        let canonical = home.canonicalize().unwrap();
        assert_eq!(refused.why, FencedWhy::HomeOutsideRoots { home: canonical });
    }

    #[test]
    fn a_home_outside_every_root_is_fenced() {
        let (_root, home) = sandbox(true);
        let other = tempfile::tempdir().unwrap();
        let refused = decide(Some(&roots(&[other.path()])), Some(&account(&home))).unwrap_err();
        assert!(
            matches!(refused.why, FencedWhy::HomeOutsideRoots { .. }),
            "{refused:?}"
        );
    }

    #[test]
    fn a_home_that_links_out_of_the_roots_is_fenced() {
        let (_outside, real_home) = sandbox(true);
        let root = tempfile::tempdir().unwrap();
        let linked = root.path().join("walker");
        symlink(&real_home, &linked).unwrap();
        let refused = decide(Some(&roots(&[root.path()])), Some(&account(&linked))).unwrap_err();
        let canonical = real_home.canonicalize().unwrap();
        assert_eq!(refused.why, FencedWhy::HomeOutsideRoots { home: canonical });
    }

    #[test]
    fn a_marker_that_is_a_link_or_a_directory_is_no_marker() {
        let (root, home) = sandbox(false);
        let elsewhere = root.path().join("marker-elsewhere");
        std::fs::write(&elsewhere, b"").unwrap();
        symlink(&elsewhere, home.join(TEST_ACCOUNT_MARKER)).unwrap();
        let refused = decide(Some(&roots(&[root.path()])), Some(&account(&home))).unwrap_err();
        assert!(
            matches!(refused.why, FencedWhy::NoMarker { .. }),
            "{refused:?}"
        );
        let (root2, home2) = sandbox(false);
        std::fs::create_dir(home2.join(TEST_ACCOUNT_MARKER)).unwrap();
        assert!(decide(Some(&roots(&[root2.path()])), Some(&account(&home2))).is_err());
    }

    #[test]
    fn a_root_that_is_no_absolute_directory_fences_everyone() {
        let (root, home) = sandbox(true);
        // `src` exists where cargo runs a package's tests: only the absolute-path rule refuses it.
        assert!(
            Path::new("src").is_dir(),
            "the unit tests run from the package root"
        );
        for bad in [
            PathBuf::from("src"),
            PathBuf::from("relative"),
            root.path().join("missing"),
            home.join(TEST_ACCOUNT_MARKER),
        ] {
            let mut list = roots(&[root.path()]);
            list.push(bad.clone());
            let refused = decide(Some(&list), Some(&account(&home))).unwrap_err();
            assert_eq!(refused.why, FencedWhy::RootUnusable { root: bad });
        }
    }

    #[test]
    fn an_empty_root_list_fences_everyone() {
        let (_root, home) = sandbox(true);
        let refused = decide(Some(&[]), Some(&account(&home))).unwrap_err();
        assert!(
            matches!(refused.why, FencedWhy::HomeOutsideRoots { .. }),
            "{refused:?}"
        );
    }

    #[test]
    fn without_an_account_the_fence_refuses() {
        let root = tempfile::tempdir().unwrap();
        let refused = decide(Some(&roots(&[root.path()])), None).unwrap_err();
        assert_eq!(
            refused,
            Fenced {
                account: "(none)".to_owned(),
                why: FencedWhy::NoAccount
            }
        );
    }

    #[test]
    fn every_refusal_names_the_variable() {
        let (root, home) = sandbox(false);
        let refused = decide(Some(&roots(&[root.path()])), Some(&account(&home))).unwrap_err();
        let text = refused.to_string();
        assert!(
            text.contains("APPRAFTER_SANDBOX") && text.contains(TEST_ACCOUNT_MARKER),
            "{text}"
        );
    }

    /// The running account — the person's, or CI's — has its home outside a fresh scratch root,
    /// so `admit` refuses before it makes anything. Even with the fence broken, this test only
    /// gets a backend it never calls.
    #[test]
    fn admit_fences_the_running_account_outside_scratch_roots() {
        let scratch = tempfile::tempdir().unwrap();
        let refused = admit(Some(&roots(&[scratch.path()]))).unwrap_err();
        let outside = matches!(
            refused.why,
            FencedWhy::HomeOutsideRoots { .. } | FencedWhy::NoAccount
        );
        assert!(outside, "{refused:?}");
    }

    /// The real backend, over a scripted libpam and a scratch record (it reaches no PAM: G-OS1's
    /// `REAL_BACKEND_SELF_TESTS`), reads the system clock: a caller's `now` two hours on neither
    /// clears the back-off nor lets a check through (critic round).
    #[test]
    fn the_real_backend_reads_its_own_clock() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        use std::time::Duration;

        use crate::pam::scripted::{ScriptedLibrary, Step};
        use crate::CheckOutcome;

        let services = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(services.path().join("etc/pam.d")).unwrap();
        let common_auth = services.path().join("etc/pam.d/common-auth");
        std::fs::write(common_auth, "auth required pam_unix.so\n").unwrap();
        let record = tempfile::tempdir().unwrap();
        std::fs::set_permissions(record.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let library = Arc::new(ScriptedLibrary::new(vec![Step::Hidden], "right"));
        let backend = SystemAccountPassword {
            inner: Checker {
                os: Os::Linux,
                account: Some(account(Path::new("/home/walker"))),
                pam: Pam::with(services.path(), Os::Linux, Box::new(Arc::clone(&library))),
                record_at: RecordAt::Dir(record.path().to_path_buf()),
            },
        };
        let wrong = backend.check(&Zeroizing::new("wrong".to_owned()), SystemTime::now());
        assert_eq!(wrong.outcome, CheckOutcome::Wrong);
        let later = SystemTime::now() + Duration::from_secs(2 * 60 * 60);
        let state = backend.backoff(later);
        assert!(matches!(state, Ok(BackoffState::Wait(_))), "{state:?}");
        let refused = backend.check(&Zeroizing::new("right".to_owned()), later);
        assert!(
            matches!(refused.outcome, CheckOutcome::Backoff(_)),
            "{refused:?}"
        );
        assert_eq!(
            library.calls().len(),
            1,
            "PAM was not asked the second time"
        );
    }
}
