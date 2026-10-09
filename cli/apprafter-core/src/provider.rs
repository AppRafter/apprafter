// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Provider reads (D.3 overview §3.7.4): the supported providers, the token format rule, and
//! the shapes of a token verification. `ping`, `verify_token` and `verification` arrive with
//! their first caller in D.3a's later tasks.

use serde::Serialize;

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

#[cfg(test)]
mod tests {
    use super::*;

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
