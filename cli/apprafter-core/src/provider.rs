// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Provider reads (D.3 overview §3.7.4): the supported providers, the token format rule, and
//! the token checks — [`ping`] (one request), [`verify_token`] (the local checks first, then
//! the ping) and [`verification`] (whoami's and doctor's row: never an error).

use std::time::{Duration, Instant};

use serde::Serialize;

use crate::context::{Context, SecretString};
use crate::error::{CoreError, CoreResult};
use crate::CancellationToken;

/// The providers a target may name — the one list both clients offer.
pub const SUPPORTED_PROVIDERS: &[&str] = &["hetzner-cloud"];

/// A token the provider accepted, and how long the check took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TokenCheck {
    pub elapsed_ms: u64,
}

/// What a token check found. Never fails: whoami and doctor show it as a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Verification {
    Verified {
        elapsed_ms: u64,
    },
    Skipped {
        reason: SkipReason,
    },
    /// The provider answered 401.
    Rejected,
    /// Any other HTTP status; `httpStatus` on the wire, never a second `status` key (the tag).
    HttpError {
        http_status: u16,
    },
    /// No answer: transport error or timeout.
    Unreachable,
}

/// Why a token was not checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The caller asked for no provider round-trip (`--no-ping`).
    NoPing,
    NoToken,
    UnsupportedProvider,
}

/// Why a Hetzner Cloud token is malformed (never the token itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenProblem {
    /// Not [`cli_core::target::HETZNER_TOKEN_LEN`] bytes; `got` is its length.
    WrongLength {
        got: usize,
    },
    NotAlphanumeric,
}

impl TokenProblem {
    /// The rule `cli_core::validate_hetzner_token_format` applies, typed.
    pub fn check(token: &str) -> Result<(), TokenProblem> {
        if token.len() != cli_core::target::HETZNER_TOKEN_LEN {
            return Err(TokenProblem::WrongLength { got: token.len() });
        }
        if !token.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(TokenProblem::NotAlphanumeric);
        }
        Ok(())
    }

    /// The snake_case name the desktop's error fields carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WrongLength { .. } => "wrong_length",
            Self::NotAlphanumeric => "not_alphanumeric",
        }
    }

    /// Exactly `validate_hetzner_token_format`'s reason.
    pub fn reason(self) -> String {
        match self {
            Self::WrongLength { got } => format!(
                "Hetzner Cloud tokens are {} ASCII alphanumeric characters; got {got}",
                cli_core::target::HETZNER_TOKEN_LEN
            ),
            Self::NotAlphanumeric => "Hetzner Cloud tokens are ASCII alphanumeric — found a \
                 non-[A-Za-z0-9] character (whitespace? a dash? something pasted with \
                 surrounding quotes?)"
                .to_string(),
        }
    }
}

/// `GET /v1/locations` with `token`: how long the provider took. 401 →
/// `CliError::ProviderTokenRejected`, anything else → `CliError::ProviderApiUnreachable`, each
/// carrying the raw error (today's classification: the add/renew goldens keep their codes) —
/// except a request that got no answer, which the client already classified (WI-453).
pub fn ping(
    ctx: &Context,
    provider: &str,
    token: &SecretString,
    cancel: &CancellationToken,
) -> CoreResult<Duration> {
    require_supported(provider)?;
    cancel.check()?;
    let started = Instant::now();
    ctx.hetzner_client(token)
        .list_locations()
        .map(|_| started.elapsed())
        .map_err(|e| CoreError::from(classify_ping_error(provider, e)))
}

/// Unknown provider, then the token's format, then [`ping`]: nothing is sent for a token that
/// cannot be right.
pub fn verify_token(
    ctx: &Context,
    provider: &str,
    token: &SecretString,
    cancel: &CancellationToken,
) -> CoreResult<TokenCheck> {
    require_supported(provider)?;
    TokenProblem::check(token.expose()).map_err(|problem| CoreError::InvalidToken { problem })?;
    let elapsed = ping(ctx, provider, token, cancel)?;
    Ok(TokenCheck {
        elapsed_ms: elapsed.as_millis() as u64,
    })
}

/// whoami's and doctor's check: never an error. A cancelled ping reads as `Unreachable`; the
/// caller checks its token right after and ends with `CoreError::Cancelled`.
pub fn verification(
    ctx: &Context,
    provider: &str,
    token: Option<&SecretString>,
    cancel: &CancellationToken,
) -> Verification {
    if ctx.no_ping() {
        return Verification::Skipped {
            reason: SkipReason::NoPing,
        };
    }
    let Some(token) = token else {
        return Verification::Skipped {
            reason: SkipReason::NoToken,
        };
    };
    if !SUPPORTED_PROVIDERS.contains(&provider) {
        return Verification::Skipped {
            reason: SkipReason::UnsupportedProvider,
        };
    }
    match ping(ctx, provider, token, cancel) {
        Ok(d) => Verification::Verified {
            elapsed_ms: d.as_millis() as u64,
        },
        Err(CoreError::Cli(cli_core::CliError::ProviderTokenRejected { .. })) => {
            Verification::Rejected
        }
        Err(CoreError::Cli(cli_core::CliError::ProviderApiUnreachable { cause, .. })) => {
            let cause: &(dyn std::error::Error + 'static) = &*cause;
            match cause.downcast_ref::<cli_core::CliError>() {
                Some(cli_core::CliError::Hetzner { status, .. }) => Verification::HttpError {
                    http_status: *status,
                },
                _ => Verification::Unreachable,
            }
        }
        Err(_) => Verification::Unreachable,
    }
}

/// `UnknownProvider` unless `provider` is in [`SUPPORTED_PROVIDERS`]. `pub(crate)`: D.3b's
/// `plan_add` and `machine::catalogue` call it by this name.
pub(crate) fn require_supported(provider: &str) -> CoreResult<()> {
    if SUPPORTED_PROVIDERS.contains(&provider) {
        Ok(())
    } else {
        Err(CoreError::UnknownProvider {
            provider: provider.to_string(),
            supported: SUPPORTED_PROVIDERS.iter().map(|p| p.to_string()).collect(),
        })
    }
}

/// The CLI's classification of a failed ping: 401 → `ProviderTokenRejected`, anything else →
/// `ProviderApiUnreachable`, the original error as the cause. A request that got no answer
/// arrives as `ProviderApiUnreachable` already — the client's classification, the one every
/// provider request shares (WI-453) — and passes through, not wrapped a second time.
fn classify_ping_error(provider: &str, err: cli_core::CliError) -> cli_core::CliError {
    match err {
        cli_core::CliError::Hetzner { status: 401, .. } => {
            cli_core::CliError::ProviderTokenRejected {
                provider: provider.to_string(),
                cause: Box::new(err),
            }
        }
        e @ cli_core::CliError::ProviderApiUnreachable { .. } => e,
        _ => cli_core::CliError::ProviderApiUnreachable {
            provider: provider.to_string(),
            cause: Box::new(err),
        },
    }
}

/// The classification of a failed provider read (the catalogue, the SKU check, the node
/// address). What the client typed passes through: an API status as `CliError::Hetzner` (its
/// status reaches the UI, bug 11), a request that got no answer as
/// `CliError::ProviderApiUnreachable` — the code the token check gives the same failure
/// (WI-453). Anything else — an answer that does not parse, a request ureq refused to send —
/// is `ProviderRequestFailed` naming the endpoint (overview §3.6.1).
pub(crate) fn read_error(e: cli_core::CliError, endpoint: &str) -> CoreError {
    match e {
        e @ (cli_core::CliError::Hetzner { .. }
        | cli_core::CliError::ProviderApiUnreachable { .. }) => CoreError::Cli(e),
        other => CoreError::ProviderRequestFailed {
            provider: "hetzner-cloud".into(),
            endpoint: endpoint.into(),
            cause: Box::new(CoreError::from(other)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{codes, CoreError, UiError};
    use crate::{CancellationToken, Context, SecretString};

    #[test]
    fn struct_variant_fields_are_camel_case_on_the_wire() {
        assert_eq!(
            serde_json::to_value(Verification::Verified { elapsed_ms: 5 }).unwrap(),
            serde_json::json!({"status":"verified","elapsedMs":5})
        );
        assert_eq!(
            serde_json::to_value(Verification::Skipped {
                reason: SkipReason::NoPing
            })
            .unwrap(),
            serde_json::json!({"status":"skipped","reason":"no_ping"})
        );
        // deviation 11: the HTTP status is `httpStatus`, never a second `status` key
        assert_eq!(
            serde_json::to_value(Verification::HttpError { http_status: 503 }).unwrap(),
            serde_json::json!({"status":"http_error","httpStatus":503})
        );
    }

    #[cfg(feature = "ts")]
    #[test]
    fn struct_variant_fields_are_camel_case_in_typescript() {
        use ts_rs::TS;
        let cfg = ts_rs::Config::new().with_large_int("number");
        let v = Verification::decl(&cfg);
        assert!(
            v.contains("elapsedMs: number") && !v.contains("elapsed_ms"),
            "{v}"
        );
        assert!(v.contains("httpStatus: number"), "{v}");
        let sku = crate::target::SkuCheck::decl(&cfg);
        assert!(sku.contains("regionWasDefault: boolean"), "{sku}");
        let fix = crate::doctor::CheckFix::decl(&cfg);
        assert!(
            fix.contains("\"kind\": \"renew_token\"") || fix.contains("kind: \"renew_token\""),
            "{fix}"
        );
    }

    fn ctx(url: &str) -> Context {
        Context::for_desktop("/tmp/unused".into(), url)
    }

    fn token() -> SecretString {
        SecretString::new("a".repeat(64))
    }

    fn locations(server: &mut mockito::Server, status: usize) -> mockito::Mock {
        server
            .mock("GET", "/v1/locations")
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(if status == 200 {
                r#"{"locations":[]}"#
            } else {
                r#"{"error":{"code":"x","message":"y"}}"#
            })
            .create()
    }

    #[test]
    fn verify_token_pings_and_times_the_answer() {
        let mut s = mockito::Server::new();
        let m = locations(&mut s, 200);
        let check = verify_token(
            &ctx(&s.url()),
            "hetzner-cloud",
            &token(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(check.elapsed_ms < 10_000);
        m.assert();
    }

    #[test]
    fn a_401_is_token_rejected_and_anything_else_unreachable() {
        let mut s = mockito::Server::new();
        let _m = locations(&mut s, 401);
        let e = ping(
            &ctx(&s.url()),
            "hetzner-cloud",
            &token(),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(
            UiError::from(&e).code.as_deref(),
            Some(codes::TOKEN_REJECTED)
        );
        let mut s = mockito::Server::new();
        let _m = locations(&mut s, 503);
        let e = ping(
            &ctx(&s.url()),
            "hetzner-cloud",
            &token(),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(
            UiError::from(&e).code.as_deref(),
            Some(codes::PROVIDER_UNREACHABLE)
        );
    }

    #[test]
    fn local_refusals_send_nothing() {
        let mut s = mockito::Server::new();
        let m = s.mock("GET", mockito::Matcher::Any).expect(0).create();
        let c = ctx(&s.url());
        assert!(matches!(
            verify_token(&c, "aws", &token(), &CancellationToken::new()),
            Err(CoreError::UnknownProvider { .. })
        ));
        assert!(matches!(
            verify_token(
                &c,
                "hetzner-cloud",
                &SecretString::new("short"),
                &CancellationToken::new()
            ),
            Err(CoreError::InvalidToken {
                problem: TokenProblem::WrongLength { got: 5 }
            })
        ));
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            ping(&c, "hetzner-cloud", &token(), &cancelled),
            Err(CoreError::Cancelled)
        ));
        m.assert();
    }

    #[test]
    fn verification_never_fails() {
        let mut s = mockito::Server::new();
        let none = s.mock("GET", mockito::Matcher::Any).expect(0).create();
        let c = ctx(&s.url());
        let cancel = CancellationToken::new();
        assert_eq!(
            verification(
                &c.clone().with_no_ping(true),
                "hetzner-cloud",
                Some(&token()),
                &cancel
            ),
            Verification::Skipped {
                reason: SkipReason::NoPing
            }
        );
        assert_eq!(
            verification(&c, "hetzner-cloud", None, &cancel),
            Verification::Skipped {
                reason: SkipReason::NoToken
            }
        );
        assert_eq!(
            verification(&c, "aws", Some(&token()), &cancel),
            Verification::Skipped {
                reason: SkipReason::UnsupportedProvider
            }
        );
        none.assert();
        // A revoked token is `Rejected`, not `Unreachable`: whoami and doctor send one to
        // rotating the token, the other to the network.
        let mut s = mockito::Server::new();
        let _m = locations(&mut s, 401);
        assert_eq!(
            verification(&ctx(&s.url()), "hetzner-cloud", Some(&token()), &cancel),
            Verification::Rejected
        );
        let mut s = mockito::Server::new();
        let _m = locations(&mut s, 503);
        assert_eq!(
            verification(&ctx(&s.url()), "hetzner-cloud", Some(&token()), &cancel),
            Verification::HttpError { http_status: 503 }
        );
        assert_eq!(
            verification(
                &ctx("http://127.0.0.1:1"),
                "hetzner-cloud",
                Some(&token()),
                &cancel
            ),
            Verification::Unreachable
        );
    }

    /// WI-453: one dead API, one classification, whichever core path found it — the wizard's
    /// catalogue and its token check, the SKU check of add and machine, a renew's ping, the
    /// node address doctor reads: `provider_unreachable`, its neutral help, and the cause
    /// naming the URL once (the catalogue read said `request_failed`, with no help).
    #[test]
    fn a_dead_api_is_provider_unreachable_on_every_core_path() {
        use crate::machine::{catalogue, check_sku, CatalogueSource};
        use crate::target::testkit::{seed_server, store_at, TOKEN_A};
        let dead = "http://127.0.0.1:1";
        let (_d, c) = store_at(&["prod"], Some("prod"), dead);
        seed_server(&c, "prod", 42, "prod-node", None);
        let prod = crate::TargetRef::named(&c, "prod").unwrap();
        let (t, cancel) = (SecretString::new(TOKEN_A), CancellationToken::new());
        let paths = [
            (
                "catalogue",
                catalogue(
                    &c,
                    CatalogueSource::Token {
                        provider: "hetzner-cloud",
                        token: &t,
                    },
                    &cancel,
                )
                .map(|_| ())
                .unwrap_err(),
                "/v1/locations",
            ),
            (
                "a target's catalogue",
                catalogue(&c, CatalogueSource::Target(&prod), &cancel)
                    .map(|_| ())
                    .unwrap_err(),
                "/v1/locations",
            ),
            (
                "the SKU check",
                check_sku(
                    &c,
                    &t,
                    "cx22",
                    "nbg1",
                    cli_core::SkuCheckFor::TargetMachine {
                        name: "prod".into(),
                    },
                    &cancel,
                )
                .unwrap_err(),
                "/v1/server_types",
            ),
            (
                "verify",
                verify_token(&c, "hetzner-cloud", &t, &cancel)
                    .map(|_| ())
                    .unwrap_err(),
                "/v1/locations",
            ),
            (
                "the node address",
                crate::target::public_address(&c, &prod, &cancel)
                    .map(|_| ())
                    .unwrap_err(),
                "/v1/servers/42",
            ),
        ];
        for (path, e, endpoint) in paths {
            let ui = UiError::from(&e);
            assert_eq!(
                ui.code.as_deref(),
                Some(codes::PROVIDER_UNREACHABLE),
                "{path}: {e:?}"
            );
            let help = ui.help.as_deref().unwrap_or_default();
            assert!(help.contains("unreachable"), "{path}: {help}");
            let url = format!("{dead}{endpoint}");
            // One cause, the transport failure itself: not wrapped a second time.
            assert_eq!(ui.causes.len(), 1, "{path}: {ui:?}");
            assert!(
                ui.causes[0].starts_with(&format!(
                    "transport error talking to {url}: Connection Failed: "
                )),
                "{path}: {ui:?}"
            );
            assert_eq!(ui.causes[0].matches(&url).count(), 1, "{path}: {ui:?}");
        }
    }

    #[test]
    fn a_token_problem_says_what_the_cli_says() {
        let cases = [
            String::new(),
            "abc".into(),
            "a".repeat(64),
            format!("{}-", "a".repeat(63)),
            format!("{} ", "a".repeat(63)),
        ];
        for t in &cases {
            assert_eq!(
                TokenProblem::check(t).map_err(|p| p.reason()),
                cli_core::validate_hetzner_token_format(t),
                "{t:?}"
            );
        }
    }
}
