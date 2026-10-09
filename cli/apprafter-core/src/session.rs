// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! whoami (D.3 overview §3.7.5): who the clients act as and which target the CLI's default
//! points at.

use serde::Serialize;

use crate::context::{Context, SecretString};
use crate::error::{CoreError, CoreResult};
use crate::provider::Verification;
use crate::ssh::SshKeyInfo;
use crate::CancellationToken;

/// Who the clients act as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Identity {
    /// No account: the self-hosted mode.
    AnonymousSelfHosted,
}

/// The CLI default target, as whoami shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WhoamiTarget {
    pub name: String,
    pub provider: String,
    pub verification: Verification,
    pub region: Option<String>,
    pub server_type: Option<String>,
    pub default_tier: Option<String>,
    pub cluster_name: Option<String>,
    pub ssh_key: Option<SshKeyInfo>,
}

/// Where the CLI's default target pointer leads. The GUI shows `Missing` as a row; the CLI
/// turns it into its `TargetNotFound` error after the identity line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CliDefaultTarget {
    None,
    Missing {
        name: String,
        available: Vec<String>,
    },
    Found {
        target: WhoamiTarget,
    },
}

/// What whoami reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WhoamiReport {
    pub identity: Identity,
    pub cli_default: CliDefaultTarget,
}

/// Who the operator is and what the CLI default is. Pings with the STORED token unless
/// `ctx.no_ping()` — never the CLI's `HCLOUD_TOKEN` override (today's CLI; R4). A dangling
/// pointer is `Missing`, never an error: the CLI turns it into `TargetNotFound`, the GUI shows a
/// row. Lockless (R3). A token cancelled by the time the verification is over ends the read
/// with [`CoreError::Cancelled`], never with a report.
pub fn whoami(ctx: &Context, cancel: &CancellationToken) -> CoreResult<WhoamiReport> {
    let identity = Identity::AnonymousSelfHosted;
    let Some(name) = crate::target::cli_default(ctx)? else {
        return Ok(WhoamiReport {
            identity,
            cli_default: CliDefaultTarget::None,
        });
    };
    let t = match cli_core::load_target(&ctx.store(), &name).map_err(CoreError::from) {
        Ok(t) => t,
        Err(CoreError::TargetNotFound { name, available }) => {
            return Ok(WhoamiReport {
                identity,
                cli_default: CliDefaultTarget::Missing { name, available },
            })
        }
        Err(e) => return Err(e),
    };
    let token = t.credentials.hetzner_token.clone().map(SecretString::new);
    let verification =
        crate::provider::verification(ctx, &t.config.provider, token.as_ref(), cancel);
    // `verification` reads a cancelled ping as `Unreachable`; its caller ends the read here.
    cancel.check()?;
    Ok(WhoamiReport {
        identity,
        cli_default: CliDefaultTarget::Found {
            target: WhoamiTarget {
                name,
                provider: t.config.provider,
                verification,
                region: t.config.region,
                server_type: t.config.server_type,
                default_tier: t.config.default_tier,
                cluster_name: t.config.cluster_name,
                ssh_key: t
                    .config
                    .ssh_key_path
                    .as_deref()
                    .map(|p| crate::ssh::inspect_key(ctx, p))
                    .transpose()?,
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::SkipReason;
    use crate::target::testkit::*;

    #[test]
    fn no_pointer_is_none_and_a_dangling_one_is_missing_with_the_list() {
        let (_d, ctx) = store(&["prod"], None);
        let c = CancellationToken::new();
        assert_eq!(
            whoami(&ctx, &c).unwrap().cli_default,
            CliDefaultTarget::None
        );
        let (_d, ctx) = store(&["prod"], Some("gone"));
        assert_eq!(
            whoami(&ctx, &c).unwrap().cli_default,
            CliDefaultTarget::Missing {
                name: "gone".into(),
                available: vec!["prod".into()]
            }
        );
    }

    #[test]
    fn no_ping_skips_and_a_ping_uses_the_stored_token_even_with_an_override() {
        let mut s = mockito::Server::new();
        let ok = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_A)
            .expect(1)
            .create();
        let (dir, _) = store_at(&["prod"], Some("prod"), &s.url());
        let env = crate::MapEnv::new()
            .with(
                "APPRAFTER_CONFIG_DIR",
                dir.path().join("store").to_str().unwrap(),
            )
            .with("APPRAFTER_HCLOUD_BASE_URL", &s.url())
            .with("HCLOUD_TOKEN", TOKEN_B); // the CLI override is NOT what whoami verifies
        let ctx = Context::from_cli_env(&env).unwrap();
        let c = CancellationToken::new();
        let skipped = whoami(&ctx.clone().with_no_ping(true), &c).unwrap();
        assert!(
            matches!(skipped.cli_default, CliDefaultTarget::Found { ref target }
            if target.verification == Verification::Skipped { reason: SkipReason::NoPing })
        );
        let pinged = whoami(&ctx, &c).unwrap();
        assert!(
            matches!(pinged.cli_default, CliDefaultTarget::Found { ref target }
            if matches!(target.verification, Verification::Verified { .. }))
        );
        ok.assert();
    }

    /// A cancelled whoami ends `Cancelled`, never a report: `verification` reads a cancelled
    /// ping as `Unreachable`, and a report would show a provider failure for a read the user
    /// cancelled. A token cancelled before the call sends nothing.
    #[test]
    fn a_cancelled_whoami_is_cancelled_not_unreachable() {
        let mut s = mockito::Server::new();
        let ping = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_A)
            .expect(0)
            .create();
        let (_d, ctx) = store_at(&["prod"], Some("prod"), &s.url());
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            matches!(whoami(&ctx, &cancel), Err(CoreError::Cancelled)),
            "{:?}",
            whoami(&ctx, &cancel)
        );
        ping.assert();
    }

    /// A cancel that lands while the ping is in flight (the ping itself checks only before it
    /// sends) still ends `Cancelled`, not with the ping's result.
    #[test]
    fn a_whoami_cancelled_during_its_ping_is_cancelled() {
        let mut s = mockito::Server::new();
        let cancel = CancellationToken::new();
        let tripped = cancel.clone();
        let _ping = route(&mut s, "/v1/locations", 200, LOCATIONS, TOKEN_A)
            .with_body_from_request(move |_| {
                tripped.cancel();
                LOCATIONS.as_bytes().to_vec()
            })
            .create();
        let (_d, ctx) = store_at(&["prod"], Some("prod"), &s.url());
        let got = whoami(&ctx, &cancel);
        assert!(matches!(got, Err(CoreError::Cancelled)), "{got:?}");
    }

    #[test]
    fn a_target_without_a_token_is_skipped_not_failed() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        edit(&ctx, "prod", |t| t.credentials.hetzner_token = None);
        let r = whoami(&ctx, &CancellationToken::new()).unwrap();
        assert!(
            matches!(r.cli_default, CliDefaultTarget::Found { ref target }
            if target.verification == Verification::Skipped { reason: SkipReason::NoToken })
        );
    }
}
