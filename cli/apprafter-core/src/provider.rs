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
/// carrying the raw error (today's classification: the add/renew goldens keep their codes).
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

/// The CLI's classification of a failed ping (`platform-cli`'s `classify_ping_error`, which
/// stays with its callers until D.3b moves `target add` onto [`ping`]): 401 →
/// `ProviderTokenRejected`, anything else → `ProviderApiUnreachable`, the original error as
/// the cause.
fn classify_ping_error(provider: &str, err: cli_core::CliError) -> cli_core::CliError {
    match err {
        cli_core::CliError::Hetzner { status: 401, .. } => {
            cli_core::CliError::ProviderTokenRejected {
                provider: provider.to_string(),
                cause: Box::new(err),
            }
        }
        _ => cli_core::CliError::ProviderApiUnreachable {
            provider: provider.to_string(),
            cause: Box::new(err),
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
