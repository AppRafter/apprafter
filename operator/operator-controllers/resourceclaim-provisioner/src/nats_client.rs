// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Imperative NATS I/O for the jetstream account model (2.5d Task 5, ADR
//! 0061 §8.1).
//!
//! Mirrors `redis_client.rs`'s two conventions:
//!
//! 1. A [`NatsAdmin`] trait so reconcile logic is unit-testable with a
//!    fake, rather than a live server — the same seam
//!    `oci_resolve::RegistryHttp` and `grace`'s injected clock use.
//! 2. Error messages that NEVER interpolate a credential. Here that
//!    matters MORE than for Redis: the NATS accounts file holds EVERY
//!    tenant's password in one file (not one Secret per tenant), and
//!    `mgr_<ns>` — the identity this module's future callers will often
//!    authenticate as — has `publish: [">"]` with no deny at all
//!    (`nats_accounts::render_account`). A logged password there is not
//!    one tenant's compromise; it is the whole namespace account's.

use async_trait::async_trait;
use thiserror::Error;

/// Errors from the imperative NATS admin path. Generic by construction —
/// `url`/`user` only, never `pass`, the same shape `RedisAdminError`
/// uses (see the module doc for why leak-safety matters more here).
#[derive(Debug, Error)]
pub enum NatsAdminError {
    /// Could not open an authenticated connection (dial, TLS, or the
    /// server's own AUTHORIZATION VIOLATION on a wrong/not-yet-synced
    /// password).
    #[error("nats connect to {url} as {user} failed: {source}")]
    Connect {
        url: String,
        user: String,
        #[source]
        source: async_nats::ConnectError,
    },
    /// The post-connect permission round trip (see [`NatsAdmin::verify_user`])
    /// failed — most often a permissions violation on the reply inbox,
    /// which is exactly the failure this trait exists to surface.
    #[error("nats verify request to {url} as {user} failed: {source}")]
    Verify {
        url: String,
        user: String,
        #[source]
        source: async_nats::RequestError,
    },
}

/// The subject every claim user's allow list grants UNCONDITIONALLY
/// (`nats_accounts::allow_list` — `"$JS.API.INFO".to_string()`, not
/// gated on `dynamicStreams` or anything else), so a request against it
/// proves both authentication AND the account's JetStream permissions
/// are live, not merely that a TCP handshake succeeded.
const VERIFY_SUBJECT: &str = "$JS.API.INFO";

/// [`NatsClient::verify_user`]'s production request/reply timeout (2.5d
/// Task 6) — deliberately chosen, NOT the `async-nats` library default
/// (`Some(Duration::from_secs(10))`, confirmed by reading
/// `async-nats-0.50.0/src/options.rs`) left in place "by omission."
///
/// A NATS permissions violation on the reply inbox delivers NO error
/// signal — the request simply never gets a reply, so it rides this
/// timeout on every single denied attempt (this crate's own standing
/// finding, first measured in the gated `verify_user_needs_the_custom_inbox_prefix`
/// integration test: a broken client took attempts × 10s to fail before
/// that test wrapped each attempt in a TEST-ONLY 1s bound). That failure
/// mode is not hypothetical in production either: `provision_nats`'s
/// step 5 calls this on the FIRST attempt right after writing a brand new
/// accounts-file entry, and the server has not necessarily finished its
/// SIGHUP reload yet — so the common case immediately after a fresh write
/// is exactly the "no reply" shape this timeout has to bound.
///
/// 2 seconds: long enough that a real request/reply round trip over a
/// cluster-internal Service (normally single-digit milliseconds) has no
/// realistic chance of a false timeout even under load, short enough that
/// `provision_nats`'s step 5 is never stalled anywhere near its own 300s
/// steady-state cadence or even its own 30s not-ready requeue interval —
/// "long enough not to thrash, short enough that a claim is not stuck
/// behind a 10s stall per reconcile," the exact two-sided constraint this
/// value was asked to satisfy.
const VERIFY_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Imperative NATS admin operations the provisioner drives against the
/// shared jetstream account model.
#[async_trait]
pub trait NatsAdmin: Send + Sync {
    /// Connect as `user` and confirm the connection is FULLY usable —
    /// authentication AND permissions — via a request/reply round trip
    /// on a subject every claim user's allow list unconditionally
    /// grants. This is what absorbs the kubelet's projected-Secret
    /// refresh lag (ADR 0061 §8.1 step 4): readiness is never a timer,
    /// it is "can this exact user actually talk to the server right
    /// now" — a plain successful TCP dial is not enough, because the
    /// server may still be serving the PREVIOUS accounts file (SIGHUP
    /// reload lag) even after this call is made with the NEW password.
    ///
    /// **`inbox_prefix` is not ceremony, and this doc says why so a
    /// later reader does not delete it as unused-looking.** A claim
    /// user's `subscribe.allow` is `[<app>.>, <inboxPrefix>.>]`
    /// (`nats_accounts::render_account`) — the DEFAULT `_INBOX.>` tree
    /// is deliberately NOT in it (ADR 0061 §4.5: subscribing to the
    /// shared inbox would let one tenant read every other tenant's
    /// in-flight replies). A client that does not set this to the
    /// claim's OWN `inboxPrefix` takes a permissions violation the
    /// instant it attempts a request/reply — which implicitly
    /// subscribes to a reply inbox under whatever prefix the client is
    /// configured with — and the provisioner concludes "not ready".
    /// That conclusion never resolves on its own: the projected-Secret
    /// lag this check exists to absorb is transient, but a wrong inbox
    /// prefix is a permanent misconfiguration, so the claim sits
    /// "not ready" FOREVER, on every claim, with no signal pointing at
    /// why. `inbox_prefix` is read exactly once downstream
    /// (`custom_inbox_prefix`), and that one call is the entire reason
    /// this function can ever return `Ok`.
    async fn verify_user(
        &self,
        url: &str,
        user: &str,
        pass: &str,
        inbox_prefix: &str,
    ) -> Result<(), NatsAdminError>;
}

/// Whether `user` can authenticate and complete a permission-scoped
/// round trip against the account (ADR 0061 §8.1 step 4). NEVER
/// propagates an error: a failed verify is exactly the transient
/// condition (kubelet Secret-projection lag, server reload lag) this
/// check exists to absorb, so it collapses to `false` — "not ready yet,
/// requeue" — rather than surfacing `NatsAdminError` as a reconcile
/// failure. Those are different outcomes and only one requeues
/// sensibly: a genuine reconcile error trips backoff/event noise for a
/// condition that resolves itself within seconds on every claim.
pub async fn user_is_ready(
    admin: &dyn NatsAdmin,
    url: &str,
    user: &str,
    pass: &str,
    inbox_prefix: &str,
) -> bool {
    admin
        .verify_user(url, user, pass, inbox_prefix)
        .await
        .is_ok()
}

/// Production [`NatsAdmin`] over the `async-nats` crate's client.
/// Connects fresh per call (an infrequent control-plane readiness
/// check, not a hot data path) — mirrors `RedisClient`'s own
/// connect-per-call shape.
#[derive(Default)]
pub struct NatsClient;

#[async_trait]
impl NatsAdmin for NatsClient {
    async fn verify_user(
        &self,
        url: &str,
        user: &str,
        pass: &str,
        inbox_prefix: &str,
    ) -> Result<(), NatsAdminError> {
        // `.user_and_password(...)` (not a `user:pass@host` URL) keeps
        // the password out of any connection string async-nats might
        // echo back in an error — the same reason `redis_client.rs`
        // builds `ConnectionInfo` directly instead of a URL.
        let client = async_nats::ConnectOptions::new()
            .user_and_password(user.to_string(), pass.to_string())
            .custom_inbox_prefix(inbox_prefix)
            .request_timeout(Some(VERIFY_REQUEST_TIMEOUT))
            .connect(url)
            .await
            .map_err(|source| NatsAdminError::Connect {
                url: url.to_string(),
                user: user.to_string(),
                source,
            })?;
        client
            .request(VERIFY_SUBJECT, Vec::new().into())
            .await
            .map(|_| ())
            .map_err(|source| NatsAdminError::Verify {
                url: url.to_string(),
                user: user.to_string(),
                source,
            })
    }
}

/// Test double for [`NatsAdmin`]. Mirrors `redis_client::FakeRedis`.
/// `#[cfg(test)]` — compiled only for this crate's own unit tests.
#[cfg(test)]
#[derive(Default)]
pub struct FakeNats {
    pub verify_calls: std::sync::Mutex<Vec<(String, String, String, String)>>,
    /// Whether the NEXT (and every subsequent) `verify_user` call
    /// fails. `false` by default — most tests want a working fake, and
    /// this reads more directly at each call site than an
    /// `Option`-typed "answer" would (there is nothing to answer with;
    /// `verify_user` only ever returns `()` on success).
    pub fails: std::sync::Mutex<bool>,
}

#[cfg(test)]
#[async_trait]
impl NatsAdmin for FakeNats {
    async fn verify_user(
        &self,
        url: &str,
        user: &str,
        pass: &str,
        inbox_prefix: &str,
    ) -> Result<(), NatsAdminError> {
        self.verify_calls.lock().unwrap().push((
            url.to_string(),
            user.to_string(),
            pass.to_string(),
            inbox_prefix.to_string(),
        ));
        if *self.fails.lock().unwrap() {
            return Err(NatsAdminError::Connect {
                url: url.to_string(),
                user: user.to_string(),
                source: async_nats::ConnectError::new(async_nats::ConnectErrorKind::TimedOut),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_failed_connect_yields_not_ready_not_an_error() {
        // "Not ready" and "error" are different outcomes and only one
        // requeues sensibly (this module's own doc on `user_is_ready`)
        // — proved here by the return TYPE being `bool`, not
        // `Result<bool, _>`: there is no error path to propagate,
        // which is the property under test, not merely a convenient
        // signature.
        let fake = FakeNats::default();
        *fake.fails.lock().unwrap() = true;
        let ready = user_is_ready(&fake, "nats://demo:4222", "u", "pw", "_INBOX_demo_app").await;
        assert!(!ready);
    }

    #[tokio::test]
    async fn a_successful_verify_yields_ready() {
        let fake = FakeNats::default();
        let ready = user_is_ready(&fake, "nats://demo:4222", "u", "pw", "_INBOX_demo_app").await;
        assert!(ready);
    }

    #[tokio::test]
    async fn fake_nats_records_each_call() {
        let fake = FakeNats::default();
        fake.verify_user("nats://demo:4222", "u", "pw", "_INBOX_demo_app")
            .await
            .unwrap();
        assert_eq!(
            fake.verify_calls.lock().unwrap().as_slice(),
            &[(
                "nats://demo:4222".to_string(),
                "u".to_string(),
                "pw".to_string(),
                "_INBOX_demo_app".to_string()
            )]
        );
    }

    #[test]
    fn error_messages_never_carry_the_password() {
        let err = NatsAdminError::Connect {
            url: "nats://demo:4222".into(),
            user: "claim_demo_feeder_jetstream".into(),
            source: async_nats::ConnectError::new(
                async_nats::ConnectErrorKind::AuthorizationViolation,
            ),
        };
        let rendered = format!("{err}");
        assert!(rendered.contains("nats://demo:4222"));
        assert!(rendered.contains("claim_demo_feeder_jetstream"));
        // The password never appears anywhere in this variant's fields
        // at all — there is no field to assert absence FROM, which is
        // the guarantee: the type itself cannot leak one.
    }

    /// Verifies ADR 0061 §4.5 for real, against a real server: a claim
    /// user's `subscribe.allow` is `[<app>.>, <inboxPrefix>.>]` — the
    /// DEFAULT `_INBOX.>` tree is NOT in it — so [`NatsClient::verify_user`]
    /// can only succeed if it sets `custom_inbox_prefix` to the claim's
    /// own prefix. Builds the accounts fragment with the REAL
    /// `nats_accounts::render_accounts_file` (not a hand-rolled
    /// parallel config, which could drift from the real format) for one
    /// claim, runs it in a real `nats:2-alpine` server (podman), and
    /// connects as that claim's user.
    ///
    /// Mutation-tested: removing `.custom_inbox_prefix(inbox_prefix)`
    /// from `NatsClient::verify_user` turns exactly this test red (see
    /// the commit message for the actual result).
    ///
    /// Run: cargo test -p operator-controllers-resourceclaim-provisioner \
    ///        verify_user_needs_the_custom_inbox_prefix -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "needs podman"]
    async fn verify_user_needs_the_custom_inbox_prefix() {
        use crate::nats_accounts::{render_accounts_file, ClaimView};

        let claim = ClaimView {
            namespace: "demo".into(),
            app: "feeder".into(),
            dynamic_streams: false,
            streams: vec![],
            consumes: vec![],
            quota_bytes: 1 << 30,
        };
        let fragment =
            render_accounts_file(std::slice::from_ref(&claim), u64::MAX, u64::MAX, &|_| {
                "verify-pw".to_string()
            })
            .expect("renders");

        let nats_conf = r#"
port: 4222
jetstream: {
  store_dir: "/tmp/nats-check-store"
}
system_account: "$SYS"
accounts: {
  "$SYS": {
    users: [
      { user: "admin", password: "check-only" }
    ]
  }
  include "accounts.conf"
}
"#;

        let dir = std::env::temp_dir().join(format!(
            "nats-verify-check-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("nats.conf"), nats_conf).expect("write nats.conf");
        std::fs::write(dir.join("accounts.conf"), &fragment).expect("write accounts.conf");

        let container = format!("nats-verify-{}", std::process::id());
        let port = 24222;
        let mount = format!("{}:/etc/nats:ro,Z", dir.display());
        let publish = format!("127.0.0.1:{port}:4222");
        let run = std::process::Command::new("podman")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &container,
                "-p",
                &publish,
                "-v",
                &mount,
                "nats:2-alpine",
                "-c",
                "/etc/nats/nats.conf",
            ])
            .output()
            .expect("run podman — is it installed?");
        assert!(
            run.status.success(),
            "podman run failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );

        let url = format!("127.0.0.1:{port}");
        let user = claim.user();
        let inbox = claim.inbox_prefix();

        // Poll until the server actually accepts connections — no fixed
        // sleep; the server typically comes up in well under a second,
        // but a loaded CI host is not guaranteed to. Each ATTEMPT is
        // itself bounded to 1s: `NatsClient`'s own request timeout is now
        // the deliberate [`VERIFY_REQUEST_TIMEOUT`] (2s, 2.5d Task 6) —
        // when this test was written it was still the unset
        // async-nats library default (10s), and a permissions violation
        // on the reply inbox delivers no error at all — the request
        // simply never gets a reply — so a broken client (this test's
        // whole reason to exist) would otherwise have taken attempts ×
        // 10s to fail instead of attempts × 1s. The per-attempt 1s bound
        // here is TEST-ONLY (wrapping the call, tighter than even the
        // now-2s production default) and stays, so the mutation this
        // test exists to catch still fails for the right reason (no
        // reply ever arrives) at the same speed regardless of which
        // production timeout is configured.
        let mut ready = false;
        for _ in 0..30 {
            let attempt = NatsClient.verify_user(&url, &user, "verify-pw", &inbox);
            if let Ok(Ok(())) =
                tokio::time::timeout(std::time::Duration::from_secs(1), attempt).await
            {
                ready = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        let _ = std::process::Command::new("podman")
            .args(["stop", "-t", "1", &container])
            .output();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(ready, "verify_user never succeeded against a real server");
    }
}
