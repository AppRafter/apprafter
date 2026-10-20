// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! How a locked target store opens in the CLI (D.4a T41, T46, T61, T62; §3.4): `CliVaultAccess`,
//! the `VaultAccess` every CLI context carries (`crate::context`). A read on a locked store is
//! served by this terminal's unlock window when one is open (`crate::holder_window`, hook A at
//! the top of `session`; never in a process that prompted or holds a key, rule 9); otherwise the
//! command asks for the lock password or the recovery key at its controlling terminal — once —
//! and its answer opens this terminal's window (hook B in `Inner::key_for`, D.4a T56).
//!
//! - **Where it asks.** Only at the controlling terminal (`crate::prompt`); anywhere else a
//!   locked store is `store_locked{no_terminal}`. The prompt names the store, its targets, the
//!   command (its clap path, never argv) and who started it. Three wrong secrets are
//!   `unlock_failed`; Ctrl-C is `op::cancelled`; five silent minutes are
//!   `store_locked{timed_out}`.
//! - **Once per command.** The key a prompt opened stays in this process for the command's secret
//!   phase; every item it opened — a token, a decrypted cache, the store's recipient — for the
//!   command's lifetime, a token and a recipient keyed by the vault bytes they came from (R3), a
//!   cache by its ciphertext — and a cache the key does not open, refused again in the same
//!   words. [`VaultAccess::end_phase`] drops the key (zeroed: `DataKey`'s drop) and keeps the
//!   items; a need that arises after it asks again — never a holder (rule 9). The end of
//!   `run()` forgets both ([`forget_all`]): the access lives in a static, which is never
//!   dropped.
//! - **Writes** take the key this process opened, else ask (T41); never a kept item.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use apprafter_core::lock::{self, OpenedVault, SlotFacts};
use apprafter_core::secrets::{KeySession, StoreId, VaultAccess, VaultRequest, VaultSession};
use apprafter_core::vault::{KdfPolicy, SlotInput};
use apprafter_core::{CoreError, SecretString, Zeroizing};
use cli_core::secrets::{CacheKeyKind, CachedSecret, Recipient, UndecryptableWhy};
use cli_core::{CliError, LockedWhy};

use crate::context::into_cli_error;
use crate::prompt::{Asker, PromptError, Terminal};

/// How many wrong secrets one prompt takes (sudo's `passwd_tries`).
pub(crate) const ATTEMPTS: u32 = 3;

/// The inline prompt's question where its answer opens no window (surface C28: the next command
/// asks again); `holder_window::inline_prompt` asks C28's other form when it will.
pub(crate) const INLINE_PROMPT: &str =
    "Lock password or recovery key (asked again by the next command): ";

/// What a wrong secret is told (surface C29).
pub(crate) const WRONG_SECRET: &str =
    "That is neither the lock password nor the recovery key of this store.\n";

/// The process's access, which every CLI context carries.
pub(crate) fn process() -> Arc<CliVaultAccess> {
    static PROCESS: OnceLock<Arc<CliVaultAccess>> = OnceLock::new();
    PROCESS
        .get_or_init(|| Arc::new(CliVaultAccess::new(Box::new(Terminal), KdfPolicy::active())))
        .clone()
}

/// What this computer tells the lock (D.4b B30): the os-slots glue's facts
/// (`crate::os_slots`) — this computer's machine id among them, so typed text also tries its
/// account-password slot (B27) — every one unknown where APPRAFTER_SANDBOX's fence refuses the
/// account (B82).
pub(crate) fn slot_facts() -> Arc<dyn SlotFacts> {
    crate::os_slots::process()
}

/// The command this process runs, as its prompt names it (`run()` records its clap path).
pub(crate) fn set_command(path: &str) {
    process().set_command(path);
}

/// The command's secret phase is over (T46): the in-process data key goes, the items stay.
pub(crate) fn end_secret_phase() {
    process().end_phase();
}

/// The command is over (T46: "or at exit"; critic finding 13): the key and every item go,
/// zeroed by their drops — the process's access is a static, and a static is never dropped.
pub(crate) fn forget_all() {
    process().forget();
}

/// A prompt that gave no answer, as the store at `root` refuses.
pub(crate) fn prompt_refusal(root: &Path) -> impl Fn(PromptError) -> CliError + '_ {
    move |e| match e {
        PromptError::NoTerminal => locked(root, LockedWhy::NoTerminal),
        PromptError::TimedOut => locked(root, LockedWhy::TimedOut),
        PromptError::Cancelled => CliError::Cancelled,
        PromptError::Io(e) => CliError::Io(e),
    }
}

fn locked(root: &Path, reason: LockedWhy) -> CliError {
    CliError::StoreLocked {
        store: root.to_path_buf(),
        reason,
        stopped_before: None,
    }
}

/// Asks at `asker`, up to [`ATTEMPTS`] times, for a secret that opens the vault of the store at
/// `root` (`id`): `intro` once (when not empty), then `prompt` per attempt. A wrong secret and a
/// mistyped recovery key are told and cost one attempt; anything else the vault says ends it.
pub(crate) fn ask_and_open(
    asker: &dyn Asker,
    root: &Path,
    id: &StoreId,
    intro: &str,
    prompt: &str,
    policy: &'static KdfPolicy,
) -> cli_core::Result<OpenedVault> {
    let refused = prompt_refusal(root);
    if !asker.present() {
        return Err(refused(PromptError::NoTerminal));
    }
    if !intro.is_empty() {
        asker.tell(intro).map_err(&refused)?;
    }
    let machine = slot_facts().machine_id().id().cloned();
    for _ in 0..ATTEMPTS {
        let input = asker.ask(prompt).map_err(&refused)?;
        match lock::open_at(
            root,
            id,
            SlotInput::from_input(input, machine.clone()),
            policy,
        ) {
            Ok(opened) => return Ok(opened),
            Err(CoreError::Cli(CliError::UnlockFailed { .. })) => {
                asker.tell(WRONG_SECRET).map_err(&refused)?
            }
            Err(CoreError::RecoveryKeyInvalid { reason }) => asker
                .tell(&format!("{}.\n", sentence(&reason.message())))
                .map_err(&refused)?,
            Err(e) => return Err(into_cli_error(e)),
        }
    }
    Err(CliError::UnlockFailed {
        store: root.to_path_buf(),
        attempts: ATTEMPTS,
        stopped_before: None,
    })
}

/// `text` with its first letter capitalised: a refusal's message as a sentence.
pub(crate) fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// T23 (critic finding 18): a test build (`insecure-fast-kdf`) says so on stderr — on `lock
/// on` here, on `unlock` and `lock status` with d4a-e — in the overview's exact words, the line
/// the golden harness drops. A release build prints nothing.
pub(crate) fn insecure_kdf_note() {
    if !KdfPolicy::active().is_release() {
        eprintln!("INSECURE-FAST-KDF: this test build derives keys with weak parameters");
    }
}

/// The CLI's `VaultAccess` (module docs).
pub(crate) struct CliVaultAccess {
    inner: Arc<Inner>,
}

struct Inner {
    asker: Box<dyn Asker>,
    policy: &'static KdfPolicy,
    held: Mutex<Held>,
    /// One prompt at a time: a second thread waits, then finds the key the first one opened.
    asking: Mutex<()>,
    home: OnceLock<Option<PathBuf>>,
    command: OnceLock<String>,
    /// Tests: the data key the last prompt opened, observed without keeping it alive — the
    /// key itself, not only its slot (critic finding 11: a clone kept elsewhere is caught).
    #[cfg(test)]
    last_key: Mutex<std::sync::Weak<apprafter_core::vault::DataKey>>,
}

#[derive(Default)]
struct Held {
    key: Option<(PathBuf, KeySession)>,
    tokens: Vec<(Arc<[u8]>, String, Option<SecretString>)>,
    caches: Vec<(String, Zeroizing<String>)>,
    unopened: Vec<Unopened>,
    recipients: Vec<(Arc<[u8]>, Recipient)>,
}

/// A cached secret the store's key did not open (`cache_undecryptable`), by its ciphertext:
/// refused again with the same words, without asking (T46).
struct Unopened {
    armored: String,
    path: String,
    detail: String,
    key: CacheKeyKind,
    reason: UndecryptableWhy,
}

impl Unopened {
    fn refusal(&self, secret: CachedSecret, target: &str) -> CliError {
        CliError::CacheUndecryptable {
            secret,
            target: target.to_string(),
            path: self.path.clone(),
            detail: self.detail.clone(),
            key: self.key,
            reason: self.reason,
        }
    }
}

impl CliVaultAccess {
    pub(crate) fn new(asker: Box<dyn Asker>, policy: &'static KdfPolicy) -> Self {
        CliVaultAccess {
            inner: Arc::new(Inner {
                asker,
                policy,
                held: Mutex::new(Held::default()),
                asking: Mutex::new(()),
                home: OnceLock::new(),
                command: OnceLock::new(),
                #[cfg(test)]
                last_key: Mutex::new(std::sync::Weak::new()),
            }),
        }
    }

    /// The home the prompt abbreviates the store's path with (`~/…`); the first one set wins.
    pub(crate) fn set_home(&self, home: Option<&Path>) {
        let _ = self.inner.home.set(home.map(Path::to_path_buf));
    }

    pub(crate) fn set_command(&self, path: &str) {
        let _ = self.inner.command.set(path.to_string());
    }

    /// D4: this process's own read session for `req`'s store — d's `CliSession`: each item
    /// from this command's kept items, else through this process's key, which may prompt
    /// once (`Inner::key_for`). Never a holder (rule 9).
    pub(crate) fn own_session(
        &self,
        req: &VaultRequest<'_>,
    ) -> cli_core::Result<Arc<dyn VaultSession>> {
        Ok(Arc::new(CliSession {
            inner: self.inner.clone(),
            root: req.store_root.to_path_buf(),
            id: req.store_id.clone(),
        }))
    }

    /// Tests: whether the data key the last prompt opened is still alive anywhere — in the
    /// slot, or in a clone something kept.
    #[cfg(test)]
    pub(crate) fn holds_key(&self) -> bool {
        self.inner
            .last_key
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upgrade()
            .is_some()
    }
}

impl Inner {
    fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn current(&self, root: &Path) -> Option<KeySession> {
        self.held()
            .key
            .as_ref()
            .filter(|(r, _)| r == root)
            .map(|(_, key)| key.clone())
    }

    /// The key this process opened for the store at `root`, else one a prompt opens now.
    fn key_for(&self, root: &Path, id: &StoreId) -> cli_core::Result<KeySession> {
        if let Some(key) = self.current(root) {
            return Ok(key);
        }
        let _one = self.asking.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(key) = self.current(root) {
            return Ok(key);
        }
        // The window shapes the question — and asks this terminal's holder for it (d4a-h F3) —
        // only where a question is shown: with no terminal, `ask_and_open` refuses before it
        // asks, and a holder looked for by this command's session is not looked for again
        // (T-lazy-2).
        let question = if self.asker.present() {
            crate::holder_window::inline_prompt(root, id, INLINE_PROMPT)
        } else {
            INLINE_PROMPT.to_string()
        };
        let opened = ask_and_open(
            &*self.asker,
            root,
            id,
            &self.intro(root),
            &question,
            self.policy,
        )?;
        crate::holder_window::after_prompt(
            root,
            id,
            &opened.key,
            opened.vault_id,
            opened.unlock_timeout_min,
        );
        #[cfg(test)]
        {
            *self.last_key.lock().unwrap_or_else(PoisonError::into_inner) =
                Arc::downgrade(&opened.key);
        }
        let key = KeySession::new(opened.key, opened.vault_id, root.to_path_buf());
        self.held().key = Some((root.to_path_buf(), key.clone()));
        Ok(key)
    }

    /// The lines before the inline prompt (surface C28, critic S10).
    fn intro(&self, root: &Path) -> String {
        let home = self.home.get().and_then(|h| h.as_deref());
        let shown = cli_core::paths::abbreviate_home(root, home);
        let names =
            cli_core::list_target_names(&cli_core::TargetStorePaths::for_root(root.to_path_buf()))
                .unwrap_or_default();
        let targets = if names.is_empty() {
            "no targets".to_string()
        } else {
            format!("targets: {}", names.join(", "))
        };
        let command = match self.command.get() {
            Some(path) if !path.is_empty() => format!("apprafter {path}"),
            _ => "apprafter".to_string(),
        };
        let who = match crate::prompt::secret::parent_process() {
            Some(p) => match p.exe {
                Some(exe) => format!("started by `{}` ({}, pid {})", p.name, exe.display(), p.pid),
                None => format!("started by `{}` (pid {})", p.name, p.pid),
            },
            None => "started by a program this computer does not name".to_string(),
        };
        // Shown through `prompt::printable` by the terminal (critic finding 14): a target
        // directory, the store's path or the parent's name cannot rewrite these lines.
        format!(
            "The target store at {shown} ({targets}) is locked, and `{command}` needs its \
             secrets.\nAsked by `{command}`, {who}.\n"
        )
    }
}

impl fmt::Debug for CliVaultAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let held = self.held();
        f.debug_struct("CliVaultAccess")
            .field("key", &held.key.as_ref().map(|_| "<held>"))
            .field(
                "items",
                &(held.tokens.len()
                    + held.caches.len()
                    + held.unopened.len()
                    + held.recipients.len()),
            )
            .finish()
    }
}

impl VaultAccess for CliVaultAccess {
    fn session(&self, req: &VaultRequest<'_>) -> cli_core::Result<Arc<dyn VaultSession>> {
        // D.4a T48, rule 9: a process that holds a key of its own never asks a holder.
        if self.inner.current(req.store_root).is_none() {
            if let Some(window) = crate::holder_window::window_session(req) {
                return Ok(window);
            }
        }
        self.own_session(req)
    }

    /// T66: a diagnostic's read — this terminal's window with no own path, never a prompt.
    fn open_session(
        &self,
        req: &VaultRequest<'_>,
    ) -> cli_core::Result<Option<Arc<dyn VaultSession>>> {
        Ok(crate::holder_window::diagnostic_session(req))
    }

    fn write_key(&self, req: &VaultRequest<'_>) -> cli_core::Result<KeySession> {
        self.inner.key_for(req.store_root, req.store_id)
    }

    fn end_phase(&self) {
        self.inner.held().key = None;
    }

    fn forget(&self) {
        *self.inner.held() = Held::default();
    }

    fn remember_cache(&self, armored: &str, plaintext: &str) {
        self.inner
            .held()
            .caches
            .push((armored.to_string(), Zeroizing::new(plaintext.to_string())));
    }
}

/// A read session: each item from this command's kept items, else through the key (which may
/// ask, once).
#[derive(Debug)]
struct CliSession {
    inner: Arc<Inner>,
    root: PathBuf,
    id: StoreId,
}

impl VaultSession for CliSession {
    fn hetzner_token(&self, vault: &[u8], target: &str) -> cli_core::Result<Option<SecretString>> {
        let kept = self
            .inner
            .held()
            .tokens
            .iter()
            .find(|(v, t, _)| v.as_ref() == vault && t == target)
            .map(|(_, _, token)| token.clone());
        if let Some(token) = kept {
            return Ok(token);
        }
        let token = self
            .inner
            .key_for(&self.root, &self.id)?
            .hetzner_token(vault, target)?;
        self.inner
            .held()
            .tokens
            .push((Arc::from(vault), target.to_string(), token.clone()));
        Ok(token)
    }

    fn open_cache(
        &self,
        vault: &[u8],
        secret: CachedSecret,
        target: &str,
        armored: &str,
    ) -> cli_core::Result<Zeroizing<String>> {
        let (kept, refused) = {
            let held = self.inner.held();
            (
                held.caches
                    .iter()
                    .find(|(a, _)| a == armored)
                    .map(|(_, plain)| plain.clone()),
                held.unopened
                    .iter()
                    .find(|u| u.armored == armored)
                    .map(|u| u.refusal(secret, target)),
            )
        };
        if let Some(plain) = kept {
            return Ok(plain);
        }
        if let Some(refusal) = refused {
            return Err(refusal);
        }
        match self
            .inner
            .key_for(&self.root, &self.id)?
            .open_cache(vault, secret, target, armored)
        {
            Ok(plain) => {
                self.inner
                    .held()
                    .caches
                    .push((armored.to_string(), plain.clone()));
                Ok(plain)
            }
            Err(CliError::CacheUndecryptable {
                secret,
                target,
                path,
                detail,
                key,
                reason,
            }) => {
                let unopened = Unopened {
                    armored: armored.to_string(),
                    path,
                    detail,
                    key,
                    reason,
                };
                let refusal = unopened.refusal(secret, &target);
                self.inner.held().unopened.push(unopened);
                Err(refusal)
            }
            Err(e) => Err(e),
        }
    }

    fn recipient(&self, vault: &[u8]) -> cli_core::Result<Recipient> {
        let kept = self
            .inner
            .held()
            .recipients
            .iter()
            .find(|(v, _)| v.as_ref() == vault)
            .map(|(_, r)| r.clone());
        if let Some(recipient) = kept {
            return Ok(recipient);
        }
        let recipient = self.inner.key_for(&self.root, &self.id)?.recipient(vault)?;
        self.inner
            .held()
            .recipients
            .push((Arc::from(vault), recipient.clone()));
        Ok(recipient)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use apprafter_core::secrets::testing::{locked_store, LockedTestStore};
    use apprafter_core::Context;

    use super::*;
    use crate::prompt::Script;

    const TOKEN: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: []\n";

    /// A locked store holding `prod` (its kubeconfig cached under the store's key).
    struct Store {
        dir: tempfile::TempDir,
        made: LockedTestStore,
        hetzner: cli_state::HetznerCloudState,
    }

    fn store() -> Store {
        let dir = tempfile::tempdir().unwrap();
        let made = locked_store(&dir.path().join("store"), &[("prod", Some(TOKEN))]);
        let sealed = cli_core::secrets::encrypt_for_recipient(YAML, made.recipient()).unwrap();
        let hetzner = serde_json::from_value(serde_json::json!({
            "server_id": 1, "server_name": "n", "kubeconfig_age": sealed,
        }))
        .unwrap();
        Store { dir, made, hetzner }
    }

    /// The store's recovery key as typed: its two lines.
    fn key(s: &Store) -> String {
        s.made.recovery_key().expose().to_string()
    }

    /// A context over `s` whose access asks `answers` as the command `kubeconfig`.
    fn context(s: &Store, script: Script) -> (Context, Arc<CliVaultAccess>, Arc<Script>) {
        let script = Arc::new(script);
        let access = Arc::new(CliVaultAccess::new(
            Box::new(Shared(script.clone())),
            apprafter_core::vault::KdfPolicy::active(),
        ));
        access.set_command("kubeconfig");
        access.set_home(Some(s.dir.path()));
        let ctx = Context::for_desktop(s.dir.path().join("store"), "http://127.0.0.1:1")
            .with_home_dir(Some(s.dir.path().join("home")))
            .with_vault_access(access.clone());
        (ctx, access, script)
    }

    /// An `Asker` the test keeps a handle on.
    struct Shared(Arc<Script>);

    impl Asker for Shared {
        fn present(&self) -> bool {
            self.0.present()
        }
        fn tell(&self, text: &str) -> Result<(), PromptError> {
            self.0.tell(text)
        }
        fn ask(&self, prompt: &str) -> Result<SecretString, PromptError> {
            self.0.ask(prompt)
        }
        fn answer(&self, prompt: &str) -> Result<String, PromptError> {
            self.0.answer(prompt)
        }
    }

    /// T46: one prompt opens every item a command needs; the prompt names the store, its
    /// targets, the command and who started it (T61, S10).
    #[test]
    fn one_prompt_opens_every_item_of_a_command() {
        let s = store();
        let (ctx, _, script) = context(&s, Script::new(vec![Ok(&key(&s))]));
        let secrets = ctx.secrets();
        assert_eq!(
            secrets.stored_token("prod").unwrap().unwrap().expose(),
            TOKEN
        );
        assert_eq!(
            secrets
                .cached_kubeconfig(&s.hetzner, "prod")
                .unwrap()
                .unwrap()
                .as_str(),
            YAML
        );
        assert_eq!(
            secrets
                .cache_key("prod")
                .unwrap()
                .recipient()
                .unwrap()
                .to_string(),
            s.made.recipient().to_string()
        );
        assert_eq!(script.asked(), vec![INLINE_PROMPT.to_string()]);
        let told = script.told();
        assert!(
            told.contains(
                "(targets: prod) is locked, and `apprafter kubeconfig` needs its secrets.\n"
            ),
            "{told}"
        );
        assert!(
            told.contains("Asked by `apprafter kubeconfig`, started by `"),
            "{told}"
        );
    }

    /// T61: three wrong secrets are `unlock_failed`, each one told.
    #[test]
    fn three_wrong_secrets_are_unlock_failed() {
        let s = store();
        let (ctx, _, script) = context(&s, Script::new(vec![Ok("one"), Ok("two"), Ok("three")]));
        match ctx.secrets().stored_token("prod").unwrap_err() {
            CliError::UnlockFailed { attempts, .. } => assert_eq!(attempts, ATTEMPTS),
            e => panic!("expected unlock_failed, got {e:?}"),
        }
        assert_eq!(script.told().matches(WRONG_SECRET).count(), 3);
    }

    /// T62: a mistyped recovery key names its group and costs one attempt.
    #[test]
    fn a_mistyped_recovery_key_names_its_group() {
        let s = store();
        let right = key(&s);
        let (prefix, data) = right.split_at(right.find('\n').unwrap() + 1);
        let mut chars: Vec<char> = data.chars().collect();
        chars[0] = if chars[0] == 'Q' { 'P' } else { 'Q' };
        let wrong = format!("{prefix}{}", chars.into_iter().collect::<String>());
        let (ctx, _, script) = context(&s, Script::new(vec![Ok(&wrong), Ok(&right)]));
        assert_eq!(
            ctx.secrets()
                .stored_token("prod")
                .unwrap()
                .unwrap()
                .expose(),
            TOKEN
        );
        assert!(
            script
                .told()
                .contains("The recovery key is mistyped near group"),
            "{}",
            script.told()
        );
    }

    /// T61: no terminal, a cancel and a silent prompt are the store's refusals.
    #[test]
    fn no_answer_is_the_stores_refusal() {
        let s = store();
        let (ctx, _, _) = context(&s, Script::absent());
        assert!(matches!(
            ctx.secrets().stored_token("prod").unwrap_err(),
            CliError::StoreLocked {
                reason: LockedWhy::NoTerminal,
                stopped_before: None,
                ..
            }
        ));
        let (ctx, _, _) = context(&s, Script::new(vec![Err(PromptError::Cancelled)]));
        assert!(matches!(
            ctx.secrets().stored_token("prod").unwrap_err(),
            CliError::Cancelled
        ));
        let (ctx, _, _) = context(&s, Script::new(vec![Err(PromptError::TimedOut)]));
        assert!(matches!(
            ctx.secrets().stored_token("prod").unwrap_err(),
            CliError::StoreLocked {
                reason: LockedWhy::TimedOut,
                ..
            }
        ));
    }

    /// T46 (critic S5): the end of the secret phase drops the key and keeps the items; an item
    /// needed after it asks again.
    #[test]
    fn the_end_of_the_phase_drops_the_key_and_keeps_the_items() {
        let s = store();
        let (ctx, access, script) = context(&s, Script::new(vec![Ok(&key(&s)), Ok(&key(&s))]));
        let secrets = ctx.secrets();
        assert_eq!(
            secrets.stored_token("prod").unwrap().unwrap().expose(),
            TOKEN
        );
        assert!(access.holds_key());
        access.end_phase();
        assert!(!access.holds_key(), "the data key is gone");
        assert_eq!(
            secrets.stored_token("prod").unwrap().unwrap().expose(),
            TOKEN
        );
        assert_eq!(script.asked().len(), 1, "a kept item asks nothing");
        assert_eq!(
            secrets
                .cached_kubeconfig(&s.hetzner, "prod")
                .unwrap()
                .unwrap()
                .as_str(),
            YAML
        );
        assert_eq!(script.asked().len(), 2, "a new item asks again");
    }

    /// T41: a vault write takes the key this process opened, else asks; never a kept item.
    #[test]
    fn a_write_takes_the_key_this_process_opened_else_asks() {
        let s = store();
        let (ctx, access, script) = context(&s, Script::new(vec![Ok(&key(&s)), Ok(&key(&s))]));
        let secrets = ctx.secrets();
        secrets.stored_token("prod").unwrap();
        secrets.prepare_write().unwrap();
        assert_eq!(script.asked().len(), 1);
        access.end_phase();
        secrets.prepare_write().unwrap();
        assert_eq!(script.asked().len(), 2);
    }

    #[test]
    fn forget_drops_the_items_too() {
        let s = store();
        let (ctx, access, script) = context(&s, Script::new(vec![Ok(&key(&s)), Ok(&key(&s))]));
        ctx.secrets().stored_token("prod").unwrap();
        access.forget();
        ctx.secrets().stored_token("prod").unwrap();
        assert_eq!(script.asked().len(), 2);
    }

    /// T46: a cache the store's key does not open is an item too — refused again in the same
    /// words, after the phase, without asking (a later step judging it, T39, costs no prompt).
    #[test]
    fn a_cache_its_key_cannot_open_is_refused_again_without_asking() {
        let s = store();
        let foreign = cli_core::secrets::encrypt_for_recipient(
            YAML,
            &cli_core::secrets::Identity::generate().to_public(),
        )
        .unwrap();
        let (ctx, access, script) = context(&s, Script::new(vec![Ok(&key(&s))]));
        let secrets = ctx.secrets();
        let refused = |e: CliError| match e {
            CliError::CacheUndecryptable { secret, target, .. } => assert_eq!(
                (secret, target.as_str()),
                (CachedSecret::ArgocdPassword, "prod")
            ),
            e => panic!("expected cache_undecryptable, got {e:?}"),
        };
        let key = secrets.cache_key("prod").unwrap();
        refused(
            secrets
                .open_cache(&key, CachedSecret::ArgocdPassword, "prod", &foreign)
                .unwrap_err(),
        );
        access.end_phase();
        refused(
            secrets
                .open_cache(&key, CachedSecret::ArgocdPassword, "prod", &foreign)
                .unwrap_err(),
        );
        assert_eq!(script.asked().len(), 1, "a kept refusal asks nothing");
    }

    /// D.4a: a cache this command wrote on a locked store is read back from the kept items,
    /// with no key held and nothing asked.
    #[test]
    fn a_cache_this_command_wrote_is_read_back_without_asking() {
        let dir = tempfile::tempdir().unwrap();
        let made = locked_store(&dir.path().join("store"), &[("prod", None)]);
        let access = CliVaultAccess::new(Box::new(Script::absent()), KdfPolicy::active());
        access.remember_cache("ARMORED", "apiVersion: v1\n");
        let session = access
            .session(&VaultRequest {
                store_root: &dir.path().join("store"),
                store_id: &made.store_id,
                slots: &[],
            })
            .unwrap();
        let plain = session
            .open_cache(b"vault", CachedSecret::Kubeconfig, "prod", "ARMORED")
            .unwrap();
        assert_eq!(plain.as_str(), "apiVersion: v1\n");
        assert!(!access.holds_key());
    }

    /// T46 (critic finding 11): `up` on a locked store asks once, in phase 0, and nothing
    /// after: the token again, the kubeconfig it caches (judging the Argo CD password cached
    /// before, T39), the read back and that password — although the data key itself is gone
    /// at the phase's end.
    #[test]
    fn up_asks_once_through_its_phases() {
        const PW: &str = "argocd-admin-password";
        const NEW_YAML: &str = "apiVersion: v1\nkind: Config\nclusters: [new]\n";
        let s = store();
        let (ctx, access, script) = context(&s, Script::new(vec![Ok(&key(&s))]));
        let paths = cli_state::StatePaths::for_active_target(&ctx.store(), "prod");
        let mut hetzner = s.hetzner.clone();
        hetzner.argocd_admin_password_age =
            Some(cli_core::secrets::encrypt_for_recipient(PW, s.made.recipient()).unwrap());
        cli_state::State {
            hetzner_cloud: Some(hetzner),
            ..Default::default()
        }
        .save(&paths)
        .unwrap();
        // Phase 0, then the phase's end (Task 13's `end_secret_phase` after it).
        crate::commands::age_cache::check_before_provisioning(
            &ctx,
            "prod",
            cli_core::ProvisionedBy::Up,
        )
        .unwrap();
        access.end_phase();
        assert!(
            !access.holds_key(),
            "the data key itself is gone, not only its slot"
        );
        let secrets = ctx.secrets();
        // Phase 1 needs the token again.
        assert_eq!(
            secrets.stored_token("prod").unwrap().unwrap().expose(),
            TOKEN
        );
        // Phase 2 caches the new kubeconfig under the store's key.
        let new = secrets
            .new_cache("prod", CachedSecret::Kubeconfig, true)
            .unwrap();
        let held = cli_core::StoreLock::exclusive(&ctx.store()).unwrap();
        secrets
            .write_cache(&held, &paths, new, NEW_YAML, false)
            .unwrap();
        drop(held);
        // Phase 3 reads it back, and the Argo CD password.
        let h = cli_state::State::load_or_default(&paths)
            .unwrap()
            .hetzner_cloud
            .unwrap();
        assert_eq!(
            secrets
                .cached_kubeconfig(&h, "prod")
                .unwrap()
                .unwrap()
                .as_str(),
            NEW_YAML
        );
        let pw = h.argocd_admin_password_age.as_deref().unwrap();
        let cache_key = secrets.cache_key("prod").unwrap();
        assert_eq!(
            secrets
                .open_cache(&cache_key, CachedSecret::ArgocdPassword, "prod", pw)
                .unwrap()
                .as_str(),
            PW
        );
        assert_eq!(script.asked().len(), 1, "one prompt for the whole run");
    }
}
