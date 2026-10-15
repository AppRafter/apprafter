// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The CLI's side of the secrets a target's state caches age-encrypted — its kubeconfig and its
//! Argo CD admin password: the prose that explains a lost age key ([`LostKey`]) and the check
//! `up` makes before it provisions. Reading and opening the caches is the shared core's
//! secret store (`Context::secrets()`, D.4a b); this module decrypts nothing.
//!
//! Two rules (WI-457):
//!
//! - **A read never creates a key** (GOTCHA-120). A new key opens nothing cached before it, so
//!   minting one on a read turns "the key is missing" into a confusing decrypt failure, and
//!   leaves a file that shadows the original when the user puts it back. A missing key is
//!   [`CliError::AgeKeyMissing`], a key that does not open the cache is
//!   [`CliError::CacheUndecryptable`]; each names the way back.
//! - **A key is created only where it costs nothing, or after the user agreed.** Nothing cached
//!   anywhere (`SecretStore::held` is empty) is a first use, and the first secret cached creates
//!   the key. Anything cached means the key it was cached under is lost: only `apprafter
//!   kubeconfig` creates a new one then, after listing what that leaves unreadable
//!   ([`LostKey`]) and asking.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use apprafter_core::secrets::SecretStore;
use apprafter_core::Zeroizing;
use cli_core::secrets::CachedSecret;
use cli_core::{CliError, ProvisionStop, ProvisionedBy, Result};
use cli_state::{HetznerCloudState, SealedTo};

pub(crate) use apprafter_core::secrets::{Held, Holding};

/// The missing-key error for `target`'s cache.
pub(crate) fn missing(target: &str, key_path: &Path) -> CliError {
    CliError::AgeKeyMissing {
        path: key_path.display().to_string(),
        target: target.to_string(),
        stopped: None,
    }
}

/// `target`'s cached kubeconfig, through the store, for a command that talks to its cluster.
/// The encrypted slot wins over the plaintext one; a missing or wrong key is the store's typed
/// error. Neither slot set is a hard error naming `apprafter kubeconfig`, never an empty
/// config: an empty `KUBECONFIG` makes kubectl fall back to `~/.kube`.
pub(crate) fn cached_kubeconfig(
    secrets: &SecretStore,
    hetzner: &HetznerCloudState,
    target: &str,
) -> Result<Zeroizing<String>> {
    secrets.cached_kubeconfig(hetzner, target)?.ok_or_else(|| {
        CliError::Other(
            "no cached kubeconfig in state; run `apprafter kubeconfig` first".to_string(),
        )
    })
}

/// The check `up` and `restore --reprovision` make before they provision anything, for
/// `target` — their phase 0 (D.4a T46). On a locked store: the store's cache key (an
/// `APPRAFTER_AGE_KEY` is refused before anything is asked), the token through the core (an
/// `HCLOUD_TOKEN` likewise; else the one prompt), the store's recipient and the target's
/// cached secrets (each opened or refused), kept by this process for the rest of the
/// command; a refusal there names the command it stopped before, so its help says nothing
/// was created (T69). In files (WI-457): their phase 2 caches the new
/// kubeconfig and never replaces a lost key (`SecretStore::new_cache`'s rule), so a lost key
/// would stop them only after a billable server was created. Here it stops them first, with
/// nothing created. A key that does not load fails here as it would there. A first use (no
/// key, nothing cached anywhere) passes: phase 2 creates the key.
pub(crate) fn check_before_provisioning(
    ctx: &apprafter_core::Context,
    target: &str,
    by: ProvisionedBy,
) -> Result<()> {
    let secrets = ctx.secrets();
    if secrets.mode()? == apprafter_core::secrets::SecretsMode::Locked {
        let stopped = |e: CliError| match e {
            CliError::StoreLocked { store, reason, .. } => CliError::StoreLocked {
                store,
                reason,
                stopped_before: Some(by),
            },
            CliError::UnlockFailed {
                store, attempts, ..
            } => CliError::UnlockFailed {
                store,
                attempts,
                stopped_before: Some(by),
            },
            other => other,
        };
        let key = secrets.cache_key(target).map_err(stopped)?;
        let named = apprafter_core::TargetRef::named(ctx, target)
            .map_err(crate::context::into_cli_error)?;
        apprafter_core::target::hetzner_token(ctx, &named)
            .map_err(|e| stopped(crate::context::into_cli_error(e)))?;
        key.recipient().map_err(stopped)?;
        // T46: the target's cached secrets too, while the key is in hand: a later phase that
        // judges one (a cache write, T39) or reads it back finds it kept, opened or refused,
        // and asks nothing after the secret phase ended.
        let paths = cli_state::StatePaths::for_active_target(&ctx.store(), target);
        if let Some(h) = cli_state::State::load_or_default(&paths)?.hetzner_cloud {
            for (secret, armored) in [
                (CachedSecret::Kubeconfig, h.kubeconfig_age),
                (CachedSecret::ArgocdPassword, h.argocd_admin_password_age),
            ] {
                let Some(armored) = armored else {
                    continue;
                };
                match secrets.open_cache(&key, secret, target, &armored) {
                    Ok(_) | Err(CliError::CacheUndecryptable { .. }) => {}
                    Err(e) => return Err(stopped(e)),
                }
            }
        }
        return Ok(());
    }
    let path = match secrets.cache_key(target) {
        Ok(_) => return Ok(()),
        Err(CliError::AgeKeyMissing { path, .. }) => path,
        Err(e) => return Err(e),
    };
    let held = secrets.held()?;
    let Some(recover) = recovery_target(&held, target) else {
        return Ok(());
    };
    Err(CliError::AgeKeyMissing {
        path,
        target: target.to_string(),
        stopped: Some(ProvisionStop::Before { by, recover }),
    })
}

/// The target whose `kubeconfig --refresh` makes the new key, from what is `held` under the
/// lost one: `target` itself when it caches a secret, else the first target that does (its
/// node answers the fetch), else the first whose state cannot be read (the refresh then names
/// what is wrong with it). `None` when nothing is held.
fn recovery_target(held: &[Held], target: &str) -> Option<String> {
    let caches = |h: &&Held| matches!(h.what, Holding::Secret(_));
    held.iter()
        .filter(caches)
        .find(|h| h.target == target)
        .or_else(|| held.iter().find(caches))
        .or_else(|| held.first())
        .map(|h| h.target.clone())
}

/// A key that is gone while something is still cached under it: what creating a new one for
/// `target`'s kubeconfig leaves unreadable. `held` comes from `SecretStore::held`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LostKey {
    pub key_path: PathBuf,
    pub target: String,
    pub held: Vec<Held>,
}

impl LostKey {
    /// F13: the ids of the target store vaults whose keys sealed what is held, when the state
    /// records one for every cached secret — `lock on` re-sealed them, and that vault is no
    /// longer here (the shared key is looked for only on a store in files). `None` when any
    /// held item records nothing or its state cannot be read: then the general words, which
    /// never claim to know.
    fn lost_vaults(&self) -> Option<Vec<&str>> {
        let mut ids: Vec<&str> = Vec::new();
        for h in &self.held {
            match (&h.what, &h.sealed_to) {
                (Holding::Secret(_), Some(SealedTo::StoreVault { store_id })) => {
                    if !ids.contains(&store_id.as_str()) {
                        ids.push(store_id);
                    }
                }
                _ => return None,
            }
        }
        (!ids.is_empty()).then_some(ids)
    }

    /// What the user reads before they agree: why the key matters, how to avoid a new one, and
    /// what a new one leaves unreadable.
    pub(crate) fn explain(&self) -> String {
        let key = self.key_path.display();
        let mut out = match self.lost_vaults() {
            Some(ids) => format!(
                "What this store caches was encrypted to the key of a target store vault that is \
                 no longer here (store id {}), not to the age key at {key}: no key put there \
                 opens it.\n\
                 If you still have that vault and its `store.yaml`, put them back and unlock \
                 instead of going on.\n",
                ids.join(", ")
            ),
            None => format!(
                "The age key at {key} is missing, so nothing cached under it can be decrypted.\n\
                 If you still have that key, put it back there, or set APPRAFTER_AGE_KEY to \
                 where it is, instead of going on.\n"
            ),
        };
        let _ = write!(
            out,
            "Going on creates a new age key at {key}, fetches the kubeconfig of target `{}` \
             from its node again over SSH, and caches it under the new key.",
            self.target
        );
        let lines = self.unreadable_lines();
        if !lines.is_empty() {
            out.push_str(match self.lost_vaults() {
                Some(_) => " The new key cannot read what the lost vault's key encrypted:\n",
                None => " The new key cannot read what the lost one encrypted:\n",
            });
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
            out.push_str(match self.lost_vaults() {
                Some(_) => "Cached under the lost vault's key, and unreadable now:\n",
                None => "Cached under the lost key, and unreadable now:\n",
            });
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

/// T45: on a locked store, a secret printed where no terminal is — a file, a pipe — is out of
/// the store's lock; one stderr line says so. `what` names it ("the kubeconfig"). Nothing when
/// stdout is a terminal, the store is in files, or its state cannot be read.
pub(crate) fn note_outside_lock(ctx: &apprafter_core::Context, what: &str) {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal()
        || !matches!(
            ctx.secrets().mode(),
            Ok(apprafter_core::secrets::SecretsMode::Locked)
        )
    {
        return;
    }
    let shown = cli_core::paths::abbreviate_home(ctx.config_root(), ctx.home_dir());
    eprintln!(
        "note: {what} went to a file or a pipe, which is outside the lock of the target store \
         at {shown}: whatever can read it can use it."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use apprafter_core::Context;
    use cli_core::secrets::load_or_create_identity;
    use cli_core::target::TargetStorePaths;
    use cli_state::StatePaths;

    /// A store at `<dir>/store` and a context over it whose age key lives under `<dir>/home`.
    fn ctx_at(dir: &Path) -> Context {
        Context::for_desktop(dir.join("store"), "http://127.0.0.1:1")
            .with_home_dir(Some(dir.join("home")))
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

    /// None cached is a hard error naming `apprafter kubeconfig`, never an empty config: an
    /// empty `KUBECONFIG` makes kubectl fall back to `~/.kube`. A plaintext slot needs no key.
    #[test]
    fn no_cached_kubeconfig_names_the_remedy_and_plaintext_needs_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());
        let err = cached_kubeconfig(&ctx.secrets(), &hetzner(None, None), "p").unwrap_err();
        assert!(
            matches!(&err, CliError::Other(m)
                if m == "no cached kubeconfig in state; run `apprafter kubeconfig` first"),
            "{err:?}"
        );
        assert_eq!(
            cached_kubeconfig(
                &ctx.secrets(),
                &hetzner(None, Some("apiVersion: v1\n")),
                "p"
            )
            .unwrap()
            .as_str(),
            "apiVersion: v1\n"
        );
        assert!(!ctx.age_key_path().exists());
    }

    /// Before `up` provisions: a key there, or a first use, passes and creates nothing. A lost
    /// key refuses, naming a target whose `kubeconfig --refresh` makes the new key: the target
    /// itself when it caches a secret, else the first that does, else the first whose state
    /// cannot be read. A key that does not load fails as it would in phase 2.
    #[test]
    fn the_check_before_provisioning_refuses_a_lost_key_and_names_a_target_to_recover() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path());
        let s = ctx.store();
        let key = ctx.age_key_path().to_path_buf();
        let check = |target: &str| check_before_provisioning(&ctx, target, ProvisionedBy::Up);
        let recover = |target: &str| match check(target) {
            Err(CliError::AgeKeyMissing {
                path,
                target: t,
                stopped:
                    Some(ProvisionStop::Before {
                        by: ProvisionedBy::Up,
                        recover,
                    }),
            }) if path == key.display().to_string() && t == target => recover,
            other => panic!("{target}: {other:?}"),
        };

        seed(&s, "prod", None);
        seed(
            &s,
            "a",
            Some(r#"{"server_id":1,"server_name":"a","kubeconfig_yaml":"plain"}"#),
        );
        check("prod").expect("a first use: nothing cached anywhere");
        assert!(!key.exists(), "the check creates no key");

        seed(&s, "b", Some("not json"));
        assert_eq!(recover("prod"), "b", "a state that cannot be read counts");
        seed(
            &s,
            "c",
            Some(r#"{"server_id":3,"server_name":"c","argocd_admin_password_age":"y"}"#),
        );
        assert_eq!(recover("prod"), "c", "a cached secret first");
        seed(
            &s,
            "d",
            Some(r#"{"server_id":4,"server_name":"d","kubeconfig_age":"x"}"#),
        );
        assert_eq!(recover("prod"), "c", "the first target that caches one");
        assert_eq!(recover("d"), "d", "the target itself when it caches one");
        assert!(!key.exists(), "a refusal creates no key");

        load_or_create_identity(&key).unwrap();
        check("prod").expect("the key is there");
        std::fs::write(&key, "not a key\n").unwrap();
        let err = check("prod").unwrap_err();
        assert!(
            matches!(&err, CliError::Other(m) if m.contains("parse age key")),
            "{err:?}"
        );
    }

    /// T46, T69: on a locked store the check is phase 0 — the token and the store's recipient,
    /// asked for once, before anything is provisioned. Where nothing can be asked it is
    /// `store_locked` naming the command it stopped before, so its help says nothing was
    /// created; with the store's key it passes and creates nothing.
    #[test]
    fn on_a_locked_store_the_check_asks_before_provisioning() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let made = apprafter_core::secrets::testing::locked_store(
            &root,
            &[(
                "prod",
                Some("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"),
            )],
        );
        let bare = ctx_at(dir.path());
        for by in [ProvisionedBy::Up, ProvisionedBy::RestoreReprovision] {
            match check_before_provisioning(&bare, "prod", by) {
                Err(CliError::StoreLocked {
                    stopped_before: Some(stopped),
                    ..
                }) => assert_eq!(stopped, by),
                other => panic!("{other:?}"),
            }
        }
        let opened = bare.clone().with_vault_access(made.access());
        check_before_provisioning(&opened, "prod", ProvisionedBy::Up).unwrap();
        assert!(
            !opened.age_key_path().exists(),
            "no shared key for a locked store"
        );
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
                    sealed_to: None,
                },
                Held {
                    target: "prod".into(),
                    what: Holding::Secret(CachedSecret::ArgocdPassword),
                    sealed_to: None,
                },
                Held {
                    target: "staging".into(),
                    what: Holding::Secret(CachedSecret::Kubeconfig),
                    sealed_to: None,
                },
                Held {
                    target: "staging".into(),
                    what: Holding::Secret(CachedSecret::ArgocdPassword),
                    sealed_to: None,
                },
                Held {
                    target: "x".into(),
                    what: Holding::Unreadable("bad json".into()),
                    sealed_to: None,
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
                sealed_to: None,
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
    /// F13: every cached secret the store holds was sealed to a target store vault's key that
    /// is no longer here (`#locked-store-lost`): the words say so, and never send the person
    /// after a shared key that held none of it. One cache that records nothing — sealed to the
    /// shared key — and the general words stand.
    #[test]
    fn a_lost_key_beside_caches_sealed_to_a_lost_vault_names_the_vault() {
        let vault = || {
            Some(SealedTo::StoreVault {
                store_id: "3f9c0a1b7d2e4c56".into(),
            })
        };
        let lost = LostKey {
            key_path: PathBuf::from("/k/age.key"),
            target: "h".into(),
            held: vec![
                Held {
                    target: "h".into(),
                    what: Holding::Secret(CachedSecret::Kubeconfig),
                    sealed_to: vault(),
                },
                Held {
                    target: "h".into(),
                    what: Holding::Secret(CachedSecret::ArgocdPassword),
                    sealed_to: vault(),
                },
            ],
        };
        let explain = lost.explain();
        for part in [
            "encrypted to the key of a target store vault that is no longer here (store id \
             3f9c0a1b7d2e4c56), not to the age key at /k/age.key: no key put there opens it.",
            "If you still have that vault and its `store.yaml`, put them back and unlock",
            "Going on creates a new age key at /k/age.key",
            "The new key cannot read what the lost vault's key encrypted:\n",
            "  - target `h`: the Argo CD admin password",
        ] {
            assert!(explain.contains(part), "{part:?} missing:\n{explain}");
        }
        assert!(
            !explain.contains("If you still have that key")
                && !explain.contains("APPRAFTER_AGE_KEY"),
            "{explain}"
        );
        assert!(
            lost.after()
                .contains("Cached under the lost vault's key, and unreadable now:\n"),
            "{}",
            lost.after()
        );

        let mixed = LostKey {
            held: vec![
                lost.held[0].clone(),
                Held {
                    sealed_to: None,
                    ..lost.held[1].clone()
                },
            ],
            ..lost.clone()
        };
        let general = mixed.explain();
        assert!(
            general.starts_with("The age key at /k/age.key is missing")
                && general.contains("If you still have that key, put it back there"),
            "{general}"
        );
        assert!(!general.contains("no longer here"), "{general}");
    }
}
