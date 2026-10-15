// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! T-lazy-3 (D.4a T7, G5; safety-net §2.5): on a locked store, what needs no secret asks the
//! client for nothing — no session, no write key, not even an open session. A counting client
//! in two contexts, the CLI's (with its holder base) and the desktop's, runs `target list`,
//! `target show`, `whoami` without a ping and `lock::status`: zero asks. The control: reading
//! the token asks once.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::testing::locked_store;
use super::*;
use crate::context::{holder_base_for, HolderOs};
use crate::lock::limits::{DisplayKind, LimitClient, SshFacts, TerminalFacts};
use crate::lock::UnlockWindow;
use crate::target_ref::TargetRef;
use crate::{CancellationToken, Context, MapEnv};

const TOKEN: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";

/// Counts every ask, and answers as a locked store with nothing open does.
#[derive(Debug, Default)]
struct Counting {
    asked: AtomicUsize,
}

impl VaultAccess for Counting {
    fn session(&self, req: &VaultRequest<'_>) -> cli_core::Result<Arc<dyn VaultSession>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Err(store_locked(req.store_root))
    }

    fn write_key(&self, req: &VaultRequest<'_>) -> cli_core::Result<KeySession> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Err(store_locked(req.store_root))
    }

    fn open_session(
        &self,
        _: &VaultRequest<'_>,
    ) -> cli_core::Result<Option<Arc<dyn VaultSession>>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}

/// The two contexts over the store at `root`, built from nothing of the real environment (G3
/// counts `from_cli_env` and `from_desktop_env` in a test as real-environment builders, and this
/// test opens the store: critic-d4a-e finding 1): the CLI's as far as these reads see it — the
/// holder base `from_cli_env` would take from an environment without `XDG_RUNTIME_DIR`, which
/// only the CLI has — and the desktop's. The KDF policy is the process's (`KdfPolicy::active`),
/// the same for both.
fn contexts(root: &Path) -> [(&'static str, Context); 2] {
    let base = holder_base_for(HolderOs::CURRENT, &MapEnv::new(), root, || None);
    let cli = Context::for_desktop(root.to_path_buf(), "http://127.0.0.1:1").with_holder_base(base);
    let desktop = Context::for_desktop(root.to_path_buf(), "http://127.0.0.1:1");
    [("cli", cli), ("desktop", desktop)]
}

#[test]
fn what_needs_no_secret_asks_the_client_for_nothing() {
    let dir = tempfile::tempdir().unwrap();
    locked_store(dir.path(), &[("prod", Some(TOKEN))]);
    cli_core::save_global_config(
        &cli_core::TargetStorePaths::for_root(dir.path().to_path_buf()),
        &cli_core::GlobalConfig {
            active_target: "prod".into(),
            version: cli_core::TARGET_STORE_VERSION,
        },
    )
    .unwrap();
    let client = LimitClient::Cli {
        terminal: Some(TerminalFacts {
            multiplexer: None,
            x_client: None,
            display: DisplayKind::None,
        }),
        ssh: SshFacts::default(),
    };
    for (who, ctx) in contexts(dir.path()) {
        let counting = Arc::new(Counting::default());
        let ctx = ctx.with_vault_access(counting.clone()).with_no_ping(true);
        crate::target::list(&ctx).unwrap();
        crate::target::show(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        crate::session::whoami(&ctx, &CancellationToken::new()).unwrap();
        crate::lock::status(&ctx, UnlockWindow::Closed, &client).unwrap();
        assert_eq!(
            counting.asked.load(Ordering::SeqCst),
            0,
            "{who}: a read that needs no secret asked the client for the vault"
        );
        // The control: the trap is armed.
        assert!(ctx.secrets().stored_token("prod").is_err());
        assert_eq!(
            counting.asked.load(Ordering::SeqCst),
            1,
            "{who}: the counter did not see a read of the token"
        );
    }
}
