// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter whoami` — one-line summary of the operator's
//! current shell context: identity, active target, verified
//! status, and the config fields most operational commands care
//! about.
//!
//! Per `cli-dx-task.md` §5.8 the layout is human-scannable rather
//! than machine-parseable; structured output (JSON, etc.) lands
//! in a later iteration when Track A.11 picks up the
//! `--output=<fmt>` flag pass.
//!
//! Deliberately small surface — no flags beyond `--no-ping` —
//! so the command stays predictable in shell prompts and CI
//! status banners.

use apprafter_core::provider::{SkipReason, Verification};
use apprafter_core::session::{self, CliDefaultTarget, Identity};
use apprafter_core::ssh::SshKeyInfo;
use apprafter_core::{CancellationToken, CoreError};
use tracing::info;

use crate::render::core_error::report;

/// `apprafter whoami` on the core's report. The ping is best-effort: a failing one does NOT
/// fail the command — operators running `whoami` on a flaky network shouldn't get an exit 1
/// when the rest of the info is still useful; it reads `verification failed ✗ — <hint>`.
pub fn run(no_ping: bool) -> miette::Result<()> {
    info!(no_ping, "whoami invoked");
    let ctx = crate::context::cli_context()?.with_no_ping(no_ping);
    // Today's order: a pointer file that cannot be read fails before the identity line.
    cli_core::resolve_active_target_name(&ctx.store(), None).map_err(miette::Report::new)?;
    let result = session::whoami(&ctx, &CancellationToken::new());
    // The identity line comes first even when the target cannot be read (today's order).
    let identity = result
        .as_ref()
        .map_or(Identity::AnonymousSelfHosted, |r| r.identity);
    println!("Identity:     {}", identity_text(&identity));
    match result.map_err(report)?.cli_default {
        CliDefaultTarget::None => {
            println!();
            println!(
                "No active target. Run `apprafter target add` to create one — `apprafter target list` to see what's configured."
            );
        }
        CliDefaultTarget::Missing { name, available } => {
            return Err(report(CoreError::TargetNotFound { name, available }));
        }
        CliDefaultTarget::Found { target: t } => {
            let or = |v: &Option<String>| v.clone().unwrap_or_else(|| "not set".into());
            println!("Target:       {} (active)", t.name);
            println!(
                "Provider:     {} ({})",
                t.provider,
                verification_text(&t.verification)
            );
            println!("Region:       {}", or(&t.region));
            println!("Server type:  {}", or(&t.server_type));
            println!("Default tier: {}", or(&t.default_tier));
            println!("Cluster name: {}", or(&t.cluster_name));
            println!("SSH key:      {}", ssh_line(t.ssh_key.as_ref()));
            println!();
            println!(
                "Run `apprafter target show` for the full target config; `apprafter target list` for all configured targets."
            );
        }
    }
    Ok(())
}

/// Identity is "anonymous (self-hosted)" until AppRafter Cloud auth lands; the report carries
/// it so that work is purely additive.
fn identity_text(i: &Identity) -> &'static str {
    match i {
        Identity::AnonymousSelfHosted => "anonymous (self-hosted mode)",
    }
}

/// The verified status on the provider line.
pub(crate) fn verification_text(v: &Verification) -> String {
    match v {
        Verification::Verified { .. } => "verified ✓".into(),
        Verification::Skipped {
            reason: SkipReason::NoPing,
        } => "verification skipped — --no-ping".into(),
        Verification::Skipped {
            reason: SkipReason::NoToken,
        } => "verification skipped — no token stored".into(),
        Verification::Skipped {
            reason: SkipReason::UnsupportedProvider,
        } => "verification skipped — provider not supported".into(),
        Verification::Rejected => "verification failed ✗ — token rejected (HTTP 401). Run `apprafter target add <name> --renew` to rotate.".into(),
        Verification::HttpError { http_status } => {
            format!("verification failed ✗ — HTTP {http_status} from provider API")
        }
        Verification::RateLimited => "verification failed ✗ — rate-limited by the provider API \
             (HTTP 429); wait, then try again"
            .into(),
        Verification::Unreachable => {
            "verification failed ✗ — provider unreachable (network?)".into()
        }
        Verification::RequestFailed => "verification failed ✗ — the provider API request failed; \
             `apprafter doctor` shows why"
            .into(),
    }
}

/// `~/.ssh/...` instead of an absolute path when the key lives under the home directory, plus a
/// `(loaded)` / `(missing!)` marker so a stale config (a deleted key file still referenced in
/// `config.yaml`) shows at a glance; `not set` for no key. A file `apply` would refuse to send
/// says so (GOTCHA-149): `(private key!)`, `(not a public key!)`, `(unreadable!)`.
pub(crate) fn ssh_line(k: Option<&SshKeyInfo>) -> String {
    use apprafter_core::ssh::SshKeyProblem;
    k.map_or_else(
        || "not set".into(),
        |k| {
            let state = match k.problem {
                None => "loaded",
                Some(SshKeyProblem::Missing) => "missing!",
                Some(SshKeyProblem::Unreadable) => "unreadable!",
                Some(SshKeyProblem::PrivateKey) => "private key!",
                Some(SshKeyProblem::NotPublicKey) => "not a public key!",
            };
            format!("{} ({state})", k.display)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use apprafter_core::provider::{SkipReason, Verification};
    use apprafter_core::ssh::SshKeyInfo;

    #[test]
    fn every_verification_reads_as_today() {
        assert_eq!(
            verification_text(&Verification::Skipped {
                reason: SkipReason::NoPing
            }),
            "verification skipped — --no-ping"
        );
        assert_eq!(
            verification_text(&Verification::Skipped {
                reason: SkipReason::NoToken
            }),
            "verification skipped — no token stored"
        );
        assert_eq!(
            verification_text(&Verification::Verified { elapsed_ms: 3 }),
            "verified ✓"
        );
        assert_eq!(
            verification_text(&Verification::Rejected),
            "verification failed ✗ — token rejected (HTTP 401). Run `apprafter target add <name> \
             --renew` to rotate."
        );
        assert_eq!(
            verification_text(&Verification::HttpError { http_status: 503 }),
            "verification failed ✗ — HTTP 503 from provider API"
        );
        assert_eq!(
            verification_text(&Verification::Unreachable),
            "verification failed ✗ — provider unreachable (network?)"
        );
        // A provider that answered is not unreachable (WI-453 follow-up).
        assert_eq!(
            verification_text(&Verification::RateLimited),
            "verification failed ✗ — rate-limited by the provider API (HTTP 429); wait, then \
             try again"
        );
        assert_eq!(
            verification_text(&Verification::RequestFailed),
            "verification failed ✗ — the provider API request failed; `apprafter doctor` shows \
             why"
        );
        assert_eq!(
            verification_text(&Verification::Skipped {
                reason: SkipReason::UnsupportedProvider
            }),
            "verification skipped — provider not supported"
        );
    }

    #[test]
    fn the_ssh_line_marks_a_missing_key_and_one_apply_would_refuse() {
        use apprafter_core::ssh::SshKeyProblem;
        let k = SshKeyInfo {
            path: "/h/.ssh/k.pub".into(),
            display: "~/.ssh/k.pub".into(),
            exists: false,
            algo: None,
            problem: Some(SshKeyProblem::Missing),
        };
        assert_eq!(ssh_line(Some(&k)), "~/.ssh/k.pub (missing!)");
        let found = |algo: Option<&str>, problem| SshKeyInfo {
            exists: true,
            algo: algo.map(String::from),
            problem,
            ..k.clone()
        };
        assert_eq!(
            ssh_line(Some(&found(Some("ssh-ed25519"), None))),
            "~/.ssh/k.pub (loaded)"
        );
        for (problem, state) in [
            (SshKeyProblem::Unreadable, "unreadable!"),
            (SshKeyProblem::PrivateKey, "private key!"),
            (SshKeyProblem::NotPublicKey, "not a public key!"),
        ] {
            assert_eq!(
                ssh_line(Some(&found(None, Some(problem)))),
                format!("~/.ssh/k.pub ({state})")
            );
        }
        assert_eq!(ssh_line(None), "not set");
    }

    #[test]
    fn the_identity_line_names_the_self_hosted_mode() {
        assert_eq!(
            identity_text(&Identity::AnonymousSelfHosted),
            "anonymous (self-hosted mode)"
        );
    }
}
