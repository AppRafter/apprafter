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
    /// The provider answered 429: it is rate-limiting requests, so the token is neither
    /// accepted nor refused yet.
    RateLimited,
    /// Any other HTTP status; `httpStatus` on the wire, never a second `status` key (the tag).
    HttpError {
        http_status: u16,
    },
    /// No answer: the name did not resolve, the connection failed or dropped, a timeout
    /// (`apprafter::target::provider_unreachable`).
    Unreachable,
    /// The request failed with no status and not for want of an answer: an answer that does not
    /// parse, or a request the HTTP client would not send (`apprafter::provider::request_failed`).
    RequestFailed,
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

/// `GET /v1/locations` with `token`: how long the provider took. A failure is classified by
/// [`classify_ping_error`]: 401 is `CliError::ProviderTokenRejected`; no answer
/// `CliError::ProviderApiUnreachable`; any other status `CliError::Hetzner` with it; an answer
/// that does not parse `CoreError::ProviderRequestFailed`.
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
        .map_err(|e| classify_ping_error(provider, e))
}

/// The request [`ping`] makes, as a failed one names it.
const PING_ENDPOINT: &str = "GET /v1/locations";

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

/// whoami's check: never an error, one [`Verification`] for each way [`ping`] classifies a
/// failure. A cancelled ping reads as `Unreachable`; the caller checks its token right after
/// and ends with `CoreError::Cancelled`.
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
    use cli_core::CliError as C;
    match ping(ctx, provider, token, cancel) {
        Ok(d) => Verification::Verified {
            elapsed_ms: d.as_millis() as u64,
        },
        Err(CoreError::Cli(C::ProviderTokenRejected { .. })) => Verification::Rejected,
        Err(CoreError::Cli(C::Hetzner { status: 429, .. })) => Verification::RateLimited,
        Err(CoreError::Cli(C::Hetzner { status, .. })) => Verification::HttpError {
            http_status: status,
        },
        Err(CoreError::Cli(C::ProviderApiUnreachable { .. }) | CoreError::Cancelled) => {
            Verification::Unreachable
        }
        Err(_) => Verification::RequestFailed,
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

/// The token check's classification of a failed ping. 401 is `ProviderTokenRejected`, the
/// original error as the cause: the one answer that is about the token. Anything else is
/// classified as every provider read is ([`read_error`], on the client's own classification of
/// a request that got no answer): only no answer is `ProviderApiUnreachable`; a provider that
/// answered keeps the code of its answer — another status `CliError::Hetzner` with it (a 429
/// among them, whose help says to wait), an answer that does not parse `ProviderRequestFailed`
/// naming the endpoint. A 429, a 5xx and an unreadable answer were all "unreachable" before,
/// which said the provider never answered (WI-453 follow-up).
fn classify_ping_error(provider: &str, err: cli_core::CliError) -> CoreError {
    match err {
        cli_core::CliError::Hetzner { status: 401, .. } => {
            CoreError::Cli(cli_core::CliError::ProviderTokenRejected {
                provider: provider.to_string(),
                cause: Box::new(err),
            })
        }
        other => read_error(other, PING_ENDPOINT),
    }
}

/// The classification of a failed provider read (the catalogue, the SKU check, the node
/// address, and the token check's ping past a 401). What the client typed passes through: an API status as `CliError::Hetzner` (its
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
        assert_eq!(
            serde_json::to_value(Verification::RateLimited).unwrap(),
            serde_json::json!({"status":"rate_limited"})
        );
        assert_eq!(
            serde_json::to_value(Verification::RequestFailed).unwrap(),
            serde_json::json!({"status":"request_failed"})
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

    /// The token check's classification. Only a 401 is about the token, and only no answer is
    /// "unreachable": a provider that answered keeps the code of what it answered (WI-453
    /// follow-up) — an error status is the Hetzner API error with its status (a 429 among them,
    /// whose help says to wait), an answer that does not parse a request failure naming the
    /// endpoint. They were all `provider_unreachable`, which said the provider never answered.
    #[test]
    fn a_401_is_token_rejected_and_an_answer_keeps_its_own_code() {
        let ping_with = |status: usize, body: &str| {
            let mut s = mockito::Server::new();
            let _m = s
                .mock("GET", "/v1/locations")
                .with_status(status)
                .with_header("content-type", "application/json")
                .with_body(body)
                .create();
            let e = ping(
                &ctx(&s.url()),
                "hetzner-cloud",
                &token(),
                &CancellationToken::new(),
            )
            .unwrap_err();
            UiError::from(&e)
        };
        let error = |code: &str, message: &str| {
            format!(r#"{{"error":{{"code":"{code}","message":"{message}"}}}}"#)
        };
        let rejected = ping_with(401, &error("unauthorized", "no"));
        assert_eq!(rejected.code.as_deref(), Some(codes::TOKEN_REJECTED));
        for (status, api_code) in [
            (503, "unavailable"),
            (500, "server_error"),
            (403, "forbidden"),
        ] {
            let ui = ping_with(status, &error(api_code, "m"));
            assert_eq!(
                ui.code.as_deref(),
                Some(codes::HETZNER_API_ERROR),
                "{status}: {ui:?}"
            );
            assert_eq!(ui.fields["status"], serde_json::json!(status), "{ui:?}");
            assert_eq!(ui.fields["apiCode"], serde_json::json!(api_code), "{ui:?}");
            assert!(
                !ui.help
                    .as_deref()
                    .unwrap_or_default()
                    .contains("unreachable"),
                "{status}: {ui:?}"
            );
        }
        // Rate-limited: the API error with its status, its help saying to wait.
        let limited = ping_with(429, &error("rate_limit_exceeded", "slow down"));
        assert_eq!(limited.code.as_deref(), Some(codes::HETZNER_API_ERROR));
        assert_eq!(limited.fields["status"], serde_json::json!(429));
        assert!(
            limited
                .help
                .as_deref()
                .unwrap_or_default()
                .contains("429 rate limit — too many requests: wait"),
            "{limited:?}"
        );
        // An answer the check cannot read: a request failure naming the endpoint, its cause the
        // parse error.
        let unreadable = ping_with(200, r#"{"nope":1}"#);
        assert_eq!(
            unreadable.code.as_deref(),
            Some(codes::PROVIDER_REQUEST_FAILED)
        );
        assert_eq!(
            unreadable.fields["endpoint"],
            serde_json::json!("GET /v1/locations")
        );
        assert!(
            unreadable.causes[0].starts_with("parse list_locations response: "),
            "{unreadable:?}"
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
        // A provider that answered is not unreachable (WI-453 follow-up): a 429 says it is
        // rate-limiting, an answer that does not parse that the request failed.
        let mut s = mockito::Server::new();
        let _m = locations(&mut s, 429);
        assert_eq!(
            verification(&ctx(&s.url()), "hetzner-cloud", Some(&token()), &cancel),
            Verification::RateLimited
        );
        let mut s = mockito::Server::new();
        let _m = s
            .mock("GET", "/v1/locations")
            .with_status(200)
            .with_body(r#"{"nope":1}"#)
            .create();
        assert_eq!(
            verification(&ctx(&s.url()), "hetzner-cloud", Some(&token()), &cancel),
            Verification::RequestFailed
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
