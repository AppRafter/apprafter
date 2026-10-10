// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Print the k3s kubeconfig for the current cluster, fetching it
//! over SSH on first use and caching the result in state.

use std::io::IsTerminal;
use std::path::Path;

use cli_core::secrets::{
    default_age_key_path, encrypt_for_recipient, load_identity, load_or_create_identity,
};
use cli_core::target::TargetStorePaths;
use cli_core::{resolve_hetzner_token, CliError, Result};
use cli_providers::hetzner_cloud::{
    default_ssh_identity_path, rewrite_server_url, HetznerCloudClient, KubeconfigFetcher,
    SshKubeconfigFetcher, APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE,
};
use cli_state::{HetznerCloudState, State, StatePaths};
use tracing::info;

use crate::commands::age_cache::{self, LostKey};
use crate::commands::hcloud::hcloud_base_url;
use crate::commands::state_paths::resolve_state_paths;

/// What to do when the age key the cache was encrypted under is lost (WI-457): a refetch then
/// needs a new key, which cannot read anything cached under the lost one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LostKeyPolicy {
    /// `apprafter kubeconfig`: list what a new key leaves unreadable, then ask at a terminal;
    /// `yes` is the answer where there is none.
    Ask { yes: bool },
    /// `up`: refuse, naming `apprafter kubeconfig --refresh`. It runs unattended, and what a new
    /// key costs is for the user to agree to.
    Refuse,
}

pub fn run(refresh: bool, yes: bool, target_override: Option<&str>) -> Result<()> {
    info!(refresh, yes, target_override, "kubeconfig invoked");
    match fetch_and_cache_outcome(refresh, target_override, LostKeyPolicy::Ask { yes })? {
        Outcome::Kubeconfig { yaml, new_key } => {
            if let Some(lost) = new_key {
                eprint!("{}", lost.after());
            }
            print!("{yaml}");
        }
        Outcome::Declined => {
            eprintln!("Cancelled: no age key was created, and nothing was fetched.");
        }
    }
    Ok(())
}

/// Resolve / fetch / cache the kubeconfig, returning the YAML
/// without printing. Split out from `run` so `bootstrap-all` (and
/// future orchestrators) can retry the cold-fetch loop in-process
/// without spawning a child or capturing stdout.
///
/// Unattended, so it never replaces a lost age key: that is [`CliError::AgeKeyMissing`], whose
/// help names `apprafter kubeconfig --refresh`, which asks first.
pub fn fetch_and_cache(refresh: bool, target_override: Option<&str>) -> Result<String> {
    match fetch_and_cache_outcome(refresh, target_override, LostKeyPolicy::Refuse)? {
        Outcome::Kubeconfig { yaml, .. } => Ok(yaml),
        Outcome::Declined => unreachable!("`Refuse` never asks, so nothing is declined"),
    }
}

/// How a fetch ended.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The kubeconfig; `new_key` when a new age key replaced a lost one for it.
    Kubeconfig {
        yaml: String,
        new_key: Option<LostKey>,
    },
    /// The user answered no to a new key: nothing was fetched or written.
    Declined,
}

fn fetch_and_cache_outcome(
    refresh: bool,
    target_override: Option<&str>,
    policy: LostKeyPolicy,
) -> Result<Outcome> {
    // Per-target state (v0.1.154). `bootstrap-all` calls into this
    // in a tight retry loop, so the migration helper inside
    // `resolve_state_paths` is no-cost after the first iteration —
    // the legacy file has already been moved.
    let resolved = resolve_state_paths(target_override)?;
    let key_path = default_age_key_path();
    let site = CacheSite {
        paths: &resolved.paths,
        store: &resolved.store,
        target: &resolved.target_name,
        key_path: &key_path,
    };
    let mut on_lost_key = |lost: &LostKey| match policy {
        LostKeyPolicy::Ask { yes } => agree_to_new_key(lost, yes),
        LostKeyPolicy::Refuse => Err(age_cache::missing(&lost.target, &lost.key_path)),
    };
    // Cold path or --refresh: SSH-fetch from the live server.
    // Credential resolution chain (cli-dx-task.md §7) — picks up
    // the active target's token when `HCLOUD_TOKEN` isn't set.
    let mut fetch = |hetzner: &HetznerCloudState| {
        let token = resolve_hetzner_token(None, &resolved.store, target_override)?;
        let client = HetznerCloudClient::new(hcloud_base_url(), token);
        let public_ip = resolve_public_ip(&client, hetzner.server_id)?;
        let fetcher = SshKubeconfigFetcher::new(
            default_ssh_identity_path(),
            resolved.paths.known_hosts_file(),
        );
        compute_kubeconfig(&fetcher, &public_ip)
    };
    fetch_and_cache_at(&site, refresh, &mut on_lost_key, &mut fetch)
}

/// The prompt for a new key: list what it leaves unreadable, then ask, at a terminal; `--yes`
/// agrees without asking. The prompt and the list go to stderr, where `inquire` draws: stdout
/// is the kubeconfig. `Ok(false)` is a no.
fn agree_to_new_key(lost: &LostKey, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    eprint!("{}", lost.explain());
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return Err(CliError::ConfirmationRequired {
            action: format!("creating a new age key at {}", lost.key_path.display()),
        });
    }
    inquire::Confirm::new("Create a new age key?")
        .with_default(false)
        .prompt()
        .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))
}

/// Where a target's kubeconfig is cached, and the age key it is cached under.
pub(crate) struct CacheSite<'a> {
    pub paths: &'a StatePaths,
    pub store: &'a TargetStorePaths,
    pub target: &'a str,
    pub key_path: &'a Path,
}

/// The kubeconfig of `site.target`: the cached copy, or (cold, or `refresh`) a fresh one from
/// its node, cached encrypted under the age key.
///
/// The cached copy is read only to be printed, never on the way to a refetch, so a lost or
/// different key cannot stop `--refresh` (GOTCHA-121). Reading it never creates a key
/// (GOTCHA-120). A refetch caches under the key there is; with none, a first use creates one,
/// and a lost one ([`age_cache::held`] is not empty) is replaced only when `on_lost_key` agrees
/// — before anything is fetched, so a refusal costs nothing. The key is created only after the
/// node answered, so a failed fetch leaves none behind; with it, this target's Argo CD password,
/// cached under the lost key, is dropped, so `argocd-password` fetches it again instead of
/// failing on it.
pub(crate) fn fetch_and_cache_at(
    site: &CacheSite<'_>,
    refresh: bool,
    on_lost_key: &mut dyn FnMut(&LostKey) -> Result<bool>,
    fetch: &mut dyn FnMut(&HetznerCloudState) -> Result<String>,
) -> Result<Outcome> {
    let mut state = State::load_or_default(site.paths)?;
    let hetzner = state.hetzner_cloud.clone().ok_or_else(|| {
        CliError::Other(
            "state has no hetzner_cloud section; run `apprafter apply` first".to_string(),
        )
    })?;

    let cached = hetzner.kubeconfig_age.is_some() || hetzner.kubeconfig_yaml.is_some();
    if cached && !refresh {
        let yaml = age_cache::cached_kubeconfig(&hetzner, site.target, site.key_path)?;
        return Ok(Outcome::Kubeconfig {
            yaml,
            new_key: None,
        });
    }

    let new_key = match load_identity(site.key_path)? {
        Some(_) => None,
        None => {
            let held = age_cache::held(site.store)?;
            if held.is_empty() {
                None
            } else {
                let lost = LostKey {
                    key_path: site.key_path.to_path_buf(),
                    target: site.target.to_string(),
                    held,
                };
                if !on_lost_key(&lost)? {
                    return Ok(Outcome::Declined);
                }
                Some(lost)
            }
        }
    };

    let yaml = fetch(&hetzner)?;

    // Encrypt before writing back; clear the legacy plaintext slot
    // so future runs go through the age path exclusively.
    let identity = load_or_create_identity(site.key_path)?;
    let armored = encrypt_for_recipient(&yaml, &identity.to_public())?;
    let mut updated = hetzner;
    updated.kubeconfig_yaml = None;
    updated.kubeconfig_age = Some(armored);
    if new_key.is_some() {
        updated.argocd_admin_password_age = None;
    }
    state.hetzner_cloud = Some(updated);
    state.save(site.paths)?;
    Ok(Outcome::Kubeconfig { yaml, new_key })
}

/// The node's kubeconfig with its loopback `server:` URL pointed at `public_ip`. Decoupled
/// from the real SSH so tests can drive it with a fake fetcher.
pub(crate) fn compute_kubeconfig<F: KubeconfigFetcher>(
    fetcher: &F,
    public_ip: &str,
) -> Result<String> {
    let raw = fetcher.fetch(public_ip)?;
    Ok(rewrite_server_url(&raw, public_ip))
}

fn resolve_public_ip(client: &HetznerCloudClient, server_id: u64) -> Result<String> {
    let resp = client.list_servers()?;
    let server = resp
        .servers
        .into_iter()
        .find(|s| {
            s.id == server_id
                && s.labels.get(APPRAFTER_LABEL).map(String::as_str) == Some(APPRAFTER_LABEL_VALUE)
        })
        .ok_or_else(|| {
            CliError::Other(format!(
                "server id {server_id} not found among apprafter-tagged servers"
            ))
        })?;
    let ip = server
        .public_net
        .and_then(|p| p.ipv4)
        .map(|v| v.ip)
        .ok_or_else(|| {
            CliError::Other(format!(
                "server id {server_id} has no public IPv4 yet — wait for cloud-init"
            ))
        })?;
    Ok(ip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use age::x25519::Identity;
    use cli_core::secrets::{decrypt_with_identity, CachedSecret};

    struct FakeFetcher {
        body: String,
        called: std::cell::Cell<u32>,
    }

    impl FakeFetcher {
        fn new(body: &str) -> Self {
            Self {
                body: body.into(),
                called: std::cell::Cell::new(0),
            }
        }
    }

    impl KubeconfigFetcher for FakeFetcher {
        fn fetch(&self, _host: &str) -> Result<String> {
            self.called.set(self.called.get() + 1);
            Ok(self.body.clone())
        }
    }

    #[test]
    fn the_fetch_rewrites_the_server_url() {
        let f = FakeFetcher::new(
            "apiVersion: v1\nclusters:\n- cluster:\n    server: https://127.0.0.1:6443\n",
        );
        let out = compute_kubeconfig(&f, "203.0.113.10").unwrap();
        assert_eq!(f.called.get(), 1);
        assert!(out.contains("server: https://203.0.113.10:6443"), "{out}");
        assert!(!out.contains("127.0.0.1"));
    }

    // ---- fetch_and_cache_at (WI-457) -----------------------------------------------------

    const FETCHED: &str = "apiVersion: v1\nfrom: node\n";

    /// A store under a tempdir: targets `prod` (whose kubeconfig is fetched) and any others the
    /// test seeds, the age key at `<dir>/key/age.key` (absent until a test makes it).
    struct World {
        _dir: tempfile::TempDir,
        store: TargetStorePaths,
        key: std::path::PathBuf,
    }

    impl World {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = TargetStorePaths::for_root(dir.path().join("store"));
            let key = dir.path().join("key/age.key");
            Self {
                _dir: dir,
                store,
                key,
            }
        }

        fn paths(&self, target: &str) -> StatePaths {
            StatePaths::for_active_target(&self.store, target)
        }

        /// Target `name` with these `hetzner_cloud` fields beside a server id and name.
        fn seed(&self, name: &str, fields: serde_json::Value) {
            let dir = self.store.root().join("targets").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("config.yaml"), "provider: hetzner-cloud\n").unwrap();
            let mut h = serde_json::json!({"server_id": 7, "server_name": name});
            h.as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let paths = self.paths(name);
            std::fs::create_dir_all(paths.state_dir()).unwrap();
            std::fs::write(
                paths.state_file(),
                serde_json::json!({ "hetzner_cloud": h }).to_string(),
            )
            .unwrap();
        }

        fn state(&self, target: &str) -> HetznerCloudState {
            State::load_or_default(&self.paths(target))
                .unwrap()
                .hetzner_cloud
                .unwrap()
        }

        fn raw_state(&self, target: &str) -> String {
            std::fs::read_to_string(self.paths(target).state_file()).unwrap()
        }

        fn key(&self) -> Identity {
            load_or_create_identity(&self.key).unwrap()
        }

        /// `fetch_and_cache_at` for `prod`, answering a lost key with `answer`; returns the
        /// outcome, the lost keys asked about and how often the node was read.
        fn run(
            &self,
            refresh: bool,
            answer: Result<bool>,
            node: Result<String>,
        ) -> (Result<Outcome>, Vec<LostKey>, u32) {
            let paths = self.paths("prod");
            let site = CacheSite {
                paths: &paths,
                store: &self.store,
                target: "prod",
                key_path: &self.key,
            };
            let mut asked = Vec::new();
            let mut answer = Some(answer);
            let mut on_lost_key = |lost: &LostKey| {
                asked.push(lost.clone());
                answer.take().expect("asked once")
            };
            let mut node = Some(node);
            let mut fetched = 0;
            let mut fetch = |_: &HetznerCloudState| {
                fetched += 1;
                node.take().expect("fetched once")
            };
            let out = fetch_and_cache_at(&site, refresh, &mut on_lost_key, &mut fetch);
            (out, asked, fetched)
        }
    }

    fn armored_for(id: &Identity, text: &str) -> String {
        encrypt_for_recipient(text, &id.to_public()).unwrap()
    }

    fn yaml(out: Result<Outcome>) -> (String, Option<LostKey>) {
        match out.unwrap() {
            Outcome::Kubeconfig { yaml, new_key } => (yaml, new_key),
            Outcome::Declined => panic!("declined"),
        }
    }

    #[test]
    fn the_cache_is_printed_without_reading_the_node() {
        let w = World::new();
        let id = w.key();
        w.seed(
            "prod",
            serde_json::json!({"kubeconfig_age": armored_for(&id, "from: cache\n")}),
        );
        let (out, asked, fetched) = w.run(false, Ok(true), Ok(FETCHED.into()));
        assert_eq!(yaml(out).0, "from: cache\n");
        assert_eq!((asked.len(), fetched), (0, 0));
    }

    #[test]
    fn refresh_reads_the_node_even_with_a_cache_and_caches_it_under_the_key() {
        let w = World::new();
        let id = w.key();
        w.seed(
            "prod",
            serde_json::json!({"kubeconfig_age": armored_for(&id, "from: cache\n"),
                               "kubeconfig_yaml": "stale: plaintext\n"}),
        );
        let (out, asked, fetched) = w.run(true, Ok(true), Ok(FETCHED.into()));
        assert_eq!(yaml(out), (FETCHED.to_string(), None));
        assert_eq!((asked.len(), fetched), (0, 1));
        let h = w.state("prod");
        assert_eq!(
            decrypt_with_identity(h.kubeconfig_age.as_deref().unwrap(), &id).unwrap(),
            FETCHED
        );
        assert!(h.kubeconfig_yaml.is_none(), "the plaintext slot is cleared");
    }

    /// GOTCHA-120: without `--refresh` a lost key is a read that fails naming the recovery —
    /// never a new key.
    #[test]
    fn a_lost_key_without_refresh_names_the_recovery_and_creates_nothing() {
        let w = World::new();
        let old = Identity::generate();
        w.seed(
            "prod",
            serde_json::json!({"kubeconfig_age": armored_for(&old, "from: cache\n")}),
        );
        let before = w.raw_state("prod");
        let (out, asked, fetched) = w.run(false, Ok(true), Ok(FETCHED.into()));
        let err = out.unwrap_err();
        assert!(
            matches!(&err, CliError::AgeKeyMissing { target, .. } if target == "prod"),
            "{err:?}"
        );
        assert_eq!((asked.len(), fetched), (0, 0));
        assert!(!w.key.exists(), "a read never creates a key");
        assert_eq!(w.raw_state("prod"), before);
    }

    /// GOTCHA-121: `--refresh` with the key lost never decrypts the cache. It asks, listing
    /// what a new key leaves unreadable, reads the node, and caches under a new key; this
    /// target's Argo CD password, cached under the lost key, is dropped.
    #[test]
    fn refresh_with_a_lost_key_asks_then_caches_under_a_new_key() {
        let w = World::new();
        let old = Identity::generate();
        w.seed(
            "prod",
            serde_json::json!({"kubeconfig_age": armored_for(&old, "from: cache\n"),
                               "argocd_admin_password_age": armored_for(&old, "pw")}),
        );
        w.seed(
            "staging",
            serde_json::json!({"kubeconfig_age": armored_for(&old, "s\n")}),
        );
        let staging_before = w.raw_state("staging");
        let (out, asked, fetched) = w.run(true, Ok(true), Ok(FETCHED.into()));
        let (got, new_key) = yaml(out);
        assert_eq!(got, FETCHED);
        assert_eq!(fetched, 1);
        assert_eq!(asked.len(), 1);
        let lost = &asked[0];
        assert_eq!(
            (lost.target.as_str(), lost.key_path.as_path()),
            ("prod", w.key.as_path())
        );
        let held: Vec<(&str, &age_cache::Holding)> = lost
            .held
            .iter()
            .map(|h| (h.target.as_str(), &h.what))
            .collect();
        use age_cache::Holding::Secret;
        assert_eq!(
            held,
            [
                ("prod", &Secret(CachedSecret::Kubeconfig)),
                ("prod", &Secret(CachedSecret::ArgocdPassword)),
                ("staging", &Secret(CachedSecret::Kubeconfig)),
            ]
        );
        assert_eq!(
            new_key.as_ref(),
            Some(lost),
            "the summary is of the same key"
        );

        let new = load_identity(&w.key).unwrap().expect("a new key");
        let h = w.state("prod");
        assert_eq!(
            decrypt_with_identity(h.kubeconfig_age.as_deref().unwrap(), &new).unwrap(),
            FETCHED
        );
        assert!(
            h.argocd_admin_password_age.is_none(),
            "the password cached under the lost key is dropped"
        );
        assert_eq!(
            w.raw_state("staging"),
            staging_before,
            "other targets untouched"
        );
    }

    #[test]
    fn a_declined_new_key_reads_and_writes_nothing() {
        let w = World::new();
        w.seed(
            "prod",
            serde_json::json!({"kubeconfig_age": armored_for(&Identity::generate(), "c\n")}),
        );
        let before = w.raw_state("prod");
        let (out, asked, fetched) = w.run(true, Ok(false), Ok(FETCHED.into()));
        assert!(matches!(out.unwrap(), Outcome::Declined));
        assert_eq!((asked.len(), fetched), (1, 0));
        assert!(!w.key.exists());
        assert_eq!(w.raw_state("prod"), before);
    }

    /// `up`'s policy: the refusal is the answer, before the node is read.
    #[test]
    fn a_refused_new_key_reads_and_writes_nothing() {
        let w = World::new();
        w.seed(
            "prod",
            serde_json::json!({"kubeconfig_age": armored_for(&Identity::generate(), "c\n")}),
        );
        let before = w.raw_state("prod");
        let refusal = age_cache::missing("prod", &w.key);
        let (out, asked, fetched) = w.run(true, Err(refusal), Ok(FETCHED.into()));
        assert!(
            matches!(out.unwrap_err(), CliError::AgeKeyMissing { .. }),
            "the refusal is the error"
        );
        assert_eq!((asked.len(), fetched), (1, 0));
        assert!(!w.key.exists());
        assert_eq!(w.raw_state("prod"), before);
    }

    #[test]
    fn a_failed_fetch_after_agreeing_leaves_no_key() {
        let w = World::new();
        w.seed(
            "prod",
            serde_json::json!({"kubeconfig_age": armored_for(&Identity::generate(), "c\n")}),
        );
        let before = w.raw_state("prod");
        let (out, asked, fetched) = w.run(
            true,
            Ok(true),
            Err(CliError::Other("ssh: connection refused".into())),
        );
        assert!(out.unwrap_err().to_string().contains("connection refused"));
        assert_eq!((asked.len(), fetched), (1, 1));
        assert!(
            !w.key.exists(),
            "the key is created only once the node answered"
        );
        assert_eq!(w.raw_state("prod"), before);
    }

    /// A key that does not open the cache (one an older CLI created on a read) is still a key:
    /// `--refresh` caches under it without asking, and leaves the password alone.
    #[test]
    fn refresh_with_a_key_that_does_not_open_the_cache_replaces_it_without_asking() {
        let w = World::new();
        let id = w.key();
        let other = Identity::generate();
        let password = armored_for(&other, "pw");
        w.seed(
            "prod",
            serde_json::json!({"kubeconfig_age": armored_for(&other, "c\n"),
                               "argocd_admin_password_age": password}),
        );
        let (out, asked, fetched) = w.run(true, Ok(true), Ok(FETCHED.into()));
        assert_eq!(yaml(out), (FETCHED.to_string(), None));
        assert_eq!((asked.len(), fetched), (0, 1));
        let h = w.state("prod");
        assert_eq!(
            decrypt_with_identity(h.kubeconfig_age.as_deref().unwrap(), &id).unwrap(),
            FETCHED
        );
        assert_eq!(
            h.argocd_admin_password_age.as_deref(),
            Some(password.as_str())
        );
        assert_eq!(
            load_identity(&w.key)
                .unwrap()
                .unwrap()
                .to_public()
                .to_string(),
            id.to_public().to_string(),
            "the key is kept"
        );
    }

    /// Nothing cached anywhere: the first fetch creates the key, as it always has.
    #[test]
    fn a_first_use_creates_the_key_without_asking() {
        let w = World::new();
        w.seed("prod", serde_json::json!({}));
        w.seed("staging", serde_json::json!({"kubeconfig_yaml": "plain\n"}));
        let (out, asked, fetched) = w.run(false, Ok(false), Ok(FETCHED.into()));
        assert_eq!(yaml(out), (FETCHED.to_string(), None));
        assert_eq!((asked.len(), fetched), (0, 1));
        let id = load_identity(&w.key).unwrap().expect("created");
        assert_eq!(
            decrypt_with_identity(w.state("prod").kubeconfig_age.as_deref().unwrap(), &id).unwrap(),
            FETCHED
        );
    }

    /// A target with no cache of its own still needs the lost key's consent: a new key would
    /// leave another target's cache unreadable.
    #[test]
    fn a_cold_fetch_beside_another_targets_cache_asks() {
        let w = World::new();
        w.seed("prod", serde_json::json!({}));
        w.seed(
            "staging",
            serde_json::json!({"argocd_admin_password_age": armored_for(&Identity::generate(), "pw")}),
        );
        let (out, asked, fetched) = w.run(false, Ok(false), Ok(FETCHED.into()));
        assert!(matches!(out.unwrap(), Outcome::Declined));
        assert_eq!((asked.len(), fetched), (1, 0));
        assert_eq!(asked[0].held.len(), 1);
        assert_eq!(asked[0].held[0].target, "staging");
        assert!(!w.key.exists());
    }
}
