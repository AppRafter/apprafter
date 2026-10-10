// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The secrets a target's state caches age-encrypted — its kubeconfig and its Argo CD admin
//! password — and the one age key they all open with (`APPRAFTER_AGE_KEY`).
//!
//! Two rules (WI-457):
//!
//! - **A read never creates a key** (GOTCHA-120). A new key opens nothing cached before it, so
//!   minting one on a read turns "the key is missing" into a confusing decrypt failure, and
//!   leaves a file that shadows the original when the user puts it back. A missing key is
//!   [`CliError::AgeKeyMissing`], a key that does not open the cache is
//!   [`CliError::CacheUndecryptable`]; each names the way back.
//! - **A key is created only where it costs nothing, or after the user agreed.** Nothing cached
//!   anywhere ([`held`] is empty) is a first use, and the first secret cached creates the key.
//!   Anything cached means the key it was cached under is lost: only `apprafter kubeconfig`
//!   creates a new one then, after listing what that leaves unreadable ([`LostKey`]) and asking.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use age::x25519::Identity;
use cli_core::secrets::{decrypt_with_identity, load_identity, CachedSecret};
use cli_core::target::{list_target_names, TargetStorePaths};
use cli_core::{CliError, Result};
use cli_state::{HetznerCloudState, State, StatePaths};

/// The missing-key error for `target`'s cache.
pub(crate) fn missing(target: &str, key_path: &Path) -> CliError {
    CliError::AgeKeyMissing {
        path: key_path.display().to_string(),
        target: target.to_string(),
    }
}

/// The key at `key_path`, which a read needs: never created.
pub(crate) fn key_for_read(target: &str, key_path: &Path) -> Result<Identity> {
    load_identity(key_path)?.ok_or_else(|| missing(target, key_path))
}

/// Decrypt `armored`, `target`'s cached `secret`, with the key at `key_path`.
pub(crate) fn decrypt(
    armored: &str,
    secret: CachedSecret,
    target: &str,
    key_path: &Path,
) -> Result<String> {
    let identity = key_for_read(target, key_path)?;
    decrypt_with_identity(armored, &identity).map_err(|e| CliError::CacheUndecryptable {
        secret,
        target: target.to_string(),
        path: key_path.display().to_string(),
        detail: e.to_string(),
    })
}

/// `target`'s cached kubeconfig, for a command that talks to its cluster.
///
/// PRECEDENCE IS LOAD-BEARING: the encrypted slot wins over the plaintext one. The plaintext
/// slot is a legacy fallback that older states may still carry beside a freshly written
/// encrypted one; preferring it would silently hand back a STALE kubeconfig for a cluster that
/// has since been re-provisioned. Neither slot set is a hard error naming `apprafter
/// kubeconfig`, never an empty config: an empty `KUBECONFIG` makes kubectl fall back to
/// `~/.kube`.
pub(crate) fn cached_kubeconfig(
    hetzner: &HetznerCloudState,
    target: &str,
    key_path: &Path,
) -> Result<String> {
    match (
        hetzner.kubeconfig_age.as_deref(),
        hetzner.kubeconfig_yaml.as_deref(),
    ) {
        (Some(armored), _) => decrypt(armored, CachedSecret::Kubeconfig, target, key_path),
        (None, Some(plain)) => Ok(plain.to_string()),
        (None, None) => Err(CliError::Other(
            "no cached kubeconfig in state; run `apprafter kubeconfig` first".to_string(),
        )),
    }
}

/// The key to cache a new secret of `target` under, for a command that never asks, WITHOUT
/// creating it: the key at `key_path`, or `None` on a first use (nothing cached anywhere), where
/// the caller creates the key once it has something to cache. With anything cached anywhere the
/// key it was cached under is lost, and the refusal names `apprafter kubeconfig --refresh`, which
/// asks. Settled before the secret is read, so a refusal costs no read (review #1).
pub(crate) fn key_for_new_secret(
    store: &TargetStorePaths,
    target: &str,
    key_path: &Path,
) -> Result<Option<Identity>> {
    if let Some(identity) = load_identity(key_path)? {
        return Ok(Some(identity));
    }
    if !held(store)?.is_empty() {
        return Err(missing(target, key_path));
    }
    Ok(None)
}

/// One thing a target's state holds under the age key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Held {
    pub target: String,
    pub what: Holding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Holding {
    Secret(CachedSecret),
    /// A state that cannot be read, which may hold either: counted as if it did, so a key is
    /// never created on the guess that it holds nothing.
    Unreadable(String),
}

/// Everything the store's targets hold encrypted, by target name, the kubeconfig before the
/// password. Read-only.
pub(crate) fn held(store: &TargetStorePaths) -> Result<Vec<Held>> {
    let mut out = Vec::new();
    for target in list_target_names(store)? {
        let paths = StatePaths::for_active_target(store, &target);
        match State::load_or_default(&paths) {
            Ok(state) => {
                let Some(h) = state.hetzner_cloud else {
                    continue;
                };
                for (slot, secret) in [
                    (&h.kubeconfig_age, CachedSecret::Kubeconfig),
                    (&h.argocd_admin_password_age, CachedSecret::ArgocdPassword),
                ] {
                    if slot.is_some() {
                        out.push(Held {
                            target: target.clone(),
                            what: Holding::Secret(secret),
                        });
                    }
                }
            }
            Err(e) => out.push(Held {
                target,
                what: Holding::Unreadable(e.to_string()),
            }),
        }
    }
    Ok(out)
}

/// A key that is gone while something is still cached under it: what creating a new one for
/// `target`'s kubeconfig leaves unreadable. `held` comes from [`held`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LostKey {
    pub key_path: PathBuf,
    pub target: String,
    pub held: Vec<Held>,
}

impl LostKey {
    /// What the user reads before they agree: why the key matters, how to avoid a new one, and
    /// what a new one leaves unreadable.
    pub(crate) fn explain(&self) -> String {
        let key = self.key_path.display();
        let mut out = format!(
            "The age key at {key} is missing, so nothing cached under it can be decrypted.\n\
             If you still have that key, put it back there, or set APPRAFTER_AGE_KEY to where \
             it is, instead of going on.\n\
             Going on creates a new age key at {key}, fetches the kubeconfig of target `{}` \
             from its node again over SSH, and caches it under the new key.",
            self.target
        );
        let lines = self.unreadable_lines();
        if !lines.is_empty() {
            out.push_str(" The new key cannot read what the lost one encrypted:\n");
            out.push_str(&lines);
        } else {
            out.push('\n');
        }
        out
    }

    /// What the user reads after the key was created: what is still to be fetched again.
    pub(crate) fn after(&self) -> String {
        let mut out = format!(
            "Created a new age key at {}; the kubeconfig of target `{}` is cached under it.\n",
            self.key_path.display(),
            self.target
        );
        let lines = self.unreadable_lines();
        if !lines.is_empty() {
            out.push_str("Cached under the lost key, and unreadable now:\n");
            out.push_str(&lines);
        }
        out
    }

    /// One line per secret the new key leaves behind, each with its way back. This target's
    /// kubeconfig is not one: it is fetched again now.
    fn unreadable_lines(&self) -> String {
        let mut out = String::new();
        for h in &self.held {
            let t = &h.target;
            let this = *t == self.target;
            let line = match (&h.what, this) {
                (Holding::Secret(CachedSecret::Kubeconfig), true) => continue,
                (Holding::Secret(CachedSecret::ArgocdPassword), true) => format!(
                    "target `{t}`: the Argo CD admin password, dropped from the cache \
                     (`apprafter argocd-password --target {t}` fetches it from the cluster again)"
                ),
                (Holding::Secret(CachedSecret::Kubeconfig), false) => format!(
                    "target `{t}`: the kubeconfig (`apprafter kubeconfig --refresh --target {t}` \
                     fetches it again)"
                ),
                (Holding::Secret(CachedSecret::ArgocdPassword), false) => format!(
                    "target `{t}`: the Argo CD admin password (`apprafter argocd-password \
                     --refresh --target {t}` fetches it again, once its kubeconfig is fetched)"
                ),
                (Holding::Unreadable(e), _) => format!(
                    "target `{t}`: its state cannot be read ({e}), so whatever it caches stays \
                     unreadable"
                ),
            };
            let _ = writeln!(out, "  - {line}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cli_core::secrets::{encrypt_for_recipient, load_or_create_identity};

    fn store(dir: &Path) -> TargetStorePaths {
        TargetStorePaths::for_root(dir.to_path_buf())
    }

    /// A target `name` in the store, with `state` (a `hetzner_cloud` object) when given.
    fn seed(store: &TargetStorePaths, name: &str, state: Option<&str>) {
        let dir = store.root().join("targets").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.yaml"), "provider: hetzner-cloud\n").unwrap();
        if let Some(h) = state {
            let paths = StatePaths::for_active_target(store, name);
            std::fs::create_dir_all(paths.state_dir()).unwrap();
            std::fs::write(paths.state_file(), format!("{{\"hetzner_cloud\":{h}}}")).unwrap();
        }
    }

    fn hetzner(kubeconfig_age: Option<&str>, kubeconfig_yaml: Option<&str>) -> HetznerCloudState {
        serde_json::from_value(serde_json::json!({
            "server_id": 1, "server_name": "n",
            "kubeconfig_age": kubeconfig_age, "kubeconfig_yaml": kubeconfig_yaml,
        }))
        .unwrap()
    }

    #[test]
    fn a_read_with_no_key_names_the_recovery_and_creates_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("cfg/age.key");
        let err = cached_kubeconfig(&hetzner(Some("armored"), None), "prod", &key).unwrap_err();
        assert!(
            matches!(&err, CliError::AgeKeyMissing { path, target }
                if *path == key.display().to_string() && target == "prod"),
            "{err:?}"
        );
        assert!(
            !key.exists() && !key.parent().unwrap().exists(),
            "a read never writes"
        );
    }

    #[test]
    fn a_key_that_does_not_open_the_cache_says_which_secret_and_whose() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("age.key");
        load_or_create_identity(&key).unwrap();
        let other = Identity::generate();
        let armored = encrypt_for_recipient("apiVersion: v1\n", &other.to_public()).unwrap();
        let err = cached_kubeconfig(&hetzner(Some(&armored), None), "prod", &key).unwrap_err();
        assert!(
            matches!(&err, CliError::CacheUndecryptable { secret: CachedSecret::Kubeconfig, target, .. }
                if target == "prod"),
            "{err:?}"
        );
        let err = decrypt(&armored, CachedSecret::ArgocdPassword, "prod", &key).unwrap_err();
        assert!(
            matches!(
                &err,
                CliError::CacheUndecryptable {
                    secret: CachedSecret::ArgocdPassword,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn the_encrypted_slot_wins_over_the_plaintext_one() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("age.key");
        let id = load_or_create_identity(&key).unwrap();
        let armored = encrypt_for_recipient("clusters: [current]\n", &id.to_public()).unwrap();
        let picked = cached_kubeconfig(
            &hetzner(Some(&armored), Some("clusters: [stale]\n")),
            "prod",
            &key,
        )
        .unwrap();
        assert_eq!(picked, "clusters: [current]\n");
    }

    #[test]
    fn plaintext_needs_no_key_and_no_slot_names_the_remedy() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("age.key");
        let plain = cached_kubeconfig(&hetzner(None, Some("apiVersion: v1\n")), "p", &key).unwrap();
        assert_eq!(plain, "apiVersion: v1\n");
        let err = cached_kubeconfig(&hetzner(None, None), "p", &key).unwrap_err();
        assert!(err.to_string().contains("apprafter kubeconfig"), "{err}");
        assert!(!key.exists());
    }

    #[test]
    fn held_lists_every_targets_ciphertexts_in_order_and_an_unreadable_state() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        seed(
            &s,
            "b",
            Some(
                r#"{"server_id":2,"server_name":"b","kubeconfig_age":"x","argocd_admin_password_age":"y"}"#,
            ),
        );
        seed(
            &s,
            "a",
            Some(r#"{"server_id":1,"server_name":"a","kubeconfig_yaml":"plain"}"#),
        );
        seed(&s, "c", None);
        seed(&s, "d", Some("not json"));
        let held = held(&s).unwrap();
        let secret = |t: &str, s| Held {
            target: t.into(),
            what: Holding::Secret(s),
        };
        assert_eq!(held.len(), 3, "{held:?}");
        assert_eq!(held[0], secret("b", CachedSecret::Kubeconfig));
        assert_eq!(held[1], secret("b", CachedSecret::ArgocdPassword));
        assert!(
            matches!(&held[2], Held { target, what: Holding::Unreadable(_) } if target == "d"),
            "{held:?}"
        );
    }

    #[test]
    fn a_first_use_has_no_key_yet_and_anything_cached_refuses_naming_the_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        seed(&s, "prod", Some(r#"{"server_id":1,"server_name":"p"}"#));
        let key = dir.path().join("k/age.key");
        assert!(
            key_for_new_secret(&s, "prod", &key).unwrap().is_none(),
            "a first use: the caller creates the key once it has something to cache"
        );
        assert!(!key.exists(), "never created here");
        let made = load_or_create_identity(&key).unwrap();
        assert_eq!(
            key_for_new_secret(&s, "prod", &key)
                .unwrap()
                .expect("the key there")
                .to_public()
                .to_string(),
            made.to_public().to_string(),
        );

        std::fs::remove_file(&key).unwrap();
        seed(
            &s,
            "staging",
            Some(r#"{"server_id":2,"server_name":"s","kubeconfig_age":"x"}"#),
        );
        let Err(err) = key_for_new_secret(&s, "prod", &key) else {
            panic!("a lost key needs asking");
        };
        assert!(
            matches!(&err, CliError::AgeKeyMissing { target, .. } if target == "prod"),
            "{err:?}"
        );
        assert!(!key.exists(), "a lost key is never replaced without asking");
    }

    #[test]
    fn a_lost_key_lists_what_a_new_one_leaves_unreadable_with_the_way_back() {
        let lost = LostKey {
            key_path: PathBuf::from("/k/age.key"),
            target: "prod".into(),
            held: vec![
                Held {
                    target: "prod".into(),
                    what: Holding::Secret(CachedSecret::Kubeconfig),
                },
                Held {
                    target: "prod".into(),
                    what: Holding::Secret(CachedSecret::ArgocdPassword),
                },
                Held {
                    target: "staging".into(),
                    what: Holding::Secret(CachedSecret::Kubeconfig),
                },
                Held {
                    target: "staging".into(),
                    what: Holding::Secret(CachedSecret::ArgocdPassword),
                },
                Held {
                    target: "x".into(),
                    what: Holding::Unreadable("bad json".into()),
                },
            ],
        };
        let explain = lost.explain();
        for part in [
            "The age key at /k/age.key is missing",
            "set APPRAFTER_AGE_KEY to where it is",
            "creates a new age key at /k/age.key",
            "fetches the kubeconfig of target `prod` from its node again over SSH",
            "  - target `prod`: the Argo CD admin password, dropped from the cache \
             (`apprafter argocd-password --target prod` fetches it from the cluster again)\n",
            "  - target `staging`: the kubeconfig (`apprafter kubeconfig --refresh --target \
             staging` fetches it again)\n",
            "  - target `staging`: the Argo CD admin password (`apprafter argocd-password \
             --refresh --target staging` fetches it again, once its kubeconfig is fetched)\n",
            "  - target `x`: its state cannot be read (bad json)",
        ] {
            assert!(explain.contains(part), "{part:?} missing:\n{explain}");
        }
        assert!(
            !explain.contains("target `prod`: the kubeconfig"),
            "this target's kubeconfig is fetched now, not left behind:\n{explain}"
        );
        let after = lost.after();
        assert!(
            after.starts_with(
                "Created a new age key at /k/age.key; the kubeconfig of target `prod` is \
                 cached under it.\nCached under the lost key, and unreadable now:\n"
            ),
            "{after}"
        );
        assert!(
            after.contains("target `staging`: the kubeconfig"),
            "{after}"
        );

        let alone = LostKey {
            held: vec![Held {
                target: "prod".into(),
                what: Holding::Secret(CachedSecret::Kubeconfig),
            }],
            ..lost
        };
        assert!(!alone.explain().contains("  - "), "{}", alone.explain());
        assert!(
            !alone.after().contains("unreadable now"),
            "{}",
            alone.after()
        );
    }
}
