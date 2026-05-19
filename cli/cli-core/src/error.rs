// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// `miette-derive` 7.6.0's generated diagnostic plumbing reassigns
// named-field bindings as part of its `Debug` / `Diagnostic` impl
// scaffolding; the initial bindings never get read and trip
// `unused_assignments` on every variant. The lint fires on
// generated code we don't control, so suppress it at file scope.
#![allow(unused_assignments)]
//! Error type used throughout `apprafter` and its libraries.
//!
//! `CliError` carries variants for every recoverable failure mode
//! the CLI surfaces. Anything we cannot recover from (programmer
//! error, broken invariants) panics.
//!
//! v0.1.86 / Track A.10 — every user-facing variant derives
//! `miette::Diagnostic` and ships a stable `code(apprafter::*)` +
//! a multi-line `help(...)` line. The binary's `main` installs
//! miette's `fancy` reporter, so unhandled `CliError`s render
//! with the rustc-quality `error:` / `help:` / `code:` block,
//! not as opaque `Debug` output. The catch-all `Other(String)`
//! still exists for call sites that haven't been promoted to a
//! typed variant yet, but new code should prefer adding a real
//! variant with its own code + help text.

use std::io;
use std::path::PathBuf;

use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum CliError {
    /// The `cue` binary was not found on `PATH`.
    #[error("`cue` binary not found on PATH")]
    #[diagnostic(
        code(apprafter::env::cue_not_found),
        help(
            "Install CUE via the project's Nix shell — `nix develop` from the repo root puts \
             `cue` on PATH automatically. If you don't have Nix, see \
             docs/contributing/setup.md for direct-install options."
        )
    )]
    CueNotFound,

    /// Calling `cue export` produced a non-zero exit code.
    #[error("cue export failed (exit {exit}): {stderr}")]
    #[diagnostic(
        code(apprafter::env::cue_export_failed),
        help(
            "The `cue export` call rejected the manifest. The captured stderr above usually \
             points at the offending CUE expression. Run `cue vet` against the manifest \
             directly to reproduce locally."
        )
    )]
    CueExport { exit: i32, stderr: String },

    /// Hetzner Cloud API call failed.
    #[error("hetzner-cloud {endpoint} failed (status {status}): {code}: {message}")]
    #[diagnostic(
        code(apprafter::provider::hetzner_api_error),
        help(
            "The Hetzner Cloud API returned a non-2xx response. Common causes:\n\
             • 401 unauthorized — the stored API token was rotated or revoked. \
               Run `apprafter target add <name> --renew --token <new>` to refresh it.\n\
             • 403 forbidden — the token's project lacks permission for this resource type.\n\
             • 429 rate limit — back off and retry; if persistent, the project may need a quota \
               increase.\n\
             • 5xx — provider-side outage; check https://status.hetzner.com/.\n\
             Re-run `apprafter doctor` to confirm reachability after fixing the root cause."
        )
    )]
    Hetzner {
        endpoint: String,
        status: u16,
        code: String,
        message: String,
    },

    /// Pre-flight rejection: the requested Hetzner server type is
    /// unknown / deprecated / unavailable in the requested region.
    /// `alternatives` carries up to 3 suggested live names for the
    /// same region.
    #[error(
        "server type `{requested}` is unavailable in region `{location}`: {reason}\n  \
         try one of: {alternatives}\n  \
         (override via APPRAFTER_MANIFEST or `--server-type` once it lands)"
    )]
    #[diagnostic(
        code(apprafter::provider::server_type_unavailable),
        help(
            "Hetzner periodically retires server types (e.g. cx22 → cpx22 in early 2026). \
             The Infrastructure manifest's `nodes[0].kind` must reference a type that is still \
             active in the chosen region. Pick one of the suggested alternatives above, or set \
             `APPRAFTER_MANIFEST` to a manifest with a different `nodes[0].kind`."
        )
    )]
    ServerTypeUnavailable {
        requested: String,
        location: String,
        reason: String,
        /// Comma-separated list of suggestions (already formatted
        /// for the error message).
        alternatives: String,
    },

    /// State file present but unparseable.
    #[error("state file at {path}: {message}")]
    #[diagnostic(
        code(apprafter::state::corrupt),
        help(
            "The local `.apprafter/state.json` file failed to parse. It was written by a \
             previous run of `apprafter apply` / `import`. If the file looks salvageable, edit \
             it by hand; otherwise delete `.apprafter/` and run `apprafter import` to rebuild \
             it from live Hetzner resources tagged with `apprafter=true`."
        )
    )]
    InvalidState { path: PathBuf, message: String },

    /// Target config / credentials / global-config file present but
    /// unparseable. Distinct from `InvalidState` so error messages
    /// can point users at `apprafter target add <name> --renew`
    /// instead of `apprafter init`.
    #[error("target config at {path}: {message}")]
    #[diagnostic(
        code(apprafter::target::invalid_config),
        help(
            "The target store YAML at the path above failed to parse. The file was either \
             hand-edited or written by an incompatible CLI version. To fix:\n\
             • If the target is recoverable, fix the YAML by hand (it's a small file).\n\
             • If not, remove just this target's directory under \
               `$XDG_CONFIG_HOME/apprafter/targets/<name>/` and re-create it with \
               `apprafter target add <name> --provider hetzner-cloud …`."
        )
    )]
    InvalidTargetConfig { path: PathBuf, message: String },

    /// A subcommand asked for target `name`, but the target store
    /// has no such target. `available` lists what *is* configured
    /// (may be empty, signalling first-run + `apprafter target add`
    /// is the right next step).
    #[error("target `{name}` not found (available: {available})")]
    #[diagnostic(
        code(apprafter::target::not_found),
        help(
            "Either the `--target` flag was given a name that's not in the store, or no target \
             has been created yet. List existing targets with `apprafter target list`; create a \
             new one with `apprafter target add <name> --provider hetzner-cloud …`. If the \
             store is empty (`available: ` shows nothing), this is your first run — start with \
             `apprafter target add`."
        )
    )]
    TargetNotFound {
        name: String,
        /// Comma-separated list of configured target names; the
        /// empty string `""` means "no targets configured yet".
        available: String,
    },

    /// Token validation ping during `target add` rejected the
    /// supplied credentials. Distinct from the generic
    /// `Hetzner { status: 401, .. }` so the operator gets a
    /// rotation-specific help text instead of the broader Hetzner
    /// API help. The underlying provider error is carried as the
    /// cause chain so miette renders both layers and operators
    /// can still see the raw API envelope.
    #[error("provider `{provider}` rejected the supplied token")]
    #[diagnostic(
        code(apprafter::target::token_rejected),
        help(
            "The provider's read-only credential check returned 401 / unauthorized. Either the \
             token was mistyped, never had the right scopes, or has been rotated / revoked \
             since you copied it.\n\
             • Verify the token at https://console.hetzner.cloud/projects → Security → API \
               Tokens. It must say `Read & Write` next to the project.\n\
             • Copy the token again (it's 64 ASCII chars, no prefix) — common cause: trailing \
               newline from a clipboard manager.\n\
             • If you're rotating, run `apprafter target add <name> --renew --token <new>` \
               instead of re-creating the target.\n\
             • Pass `--no-ping` to skip the check if you're seeding a target offline (CI \
               sandbox, intermittent network)."
        )
    )]
    ProviderTokenRejected {
        provider: String,
        #[source]
        #[diagnostic_source]
        cause: Box<dyn miette::Diagnostic + Send + Sync + 'static>,
    },

    /// Token validation ping during `target add` failed for a
    /// non-auth reason — transport error, 429, 5xx, etc. We can't
    /// rotate the user's way out of these, so the help points at
    /// `apprafter doctor` + `--no-ping`.
    #[error("provider `{provider}` API was unreachable during token validation")]
    #[diagnostic(
        code(apprafter::target::provider_unreachable),
        help(
            "The credential check could not complete because the provider's API was \
             unreachable. This is NOT a credentials problem — the token may still be valid \
             once the API recovers.\n\
             • Run `apprafter doctor` to confirm reachability + DNS.\n\
             • Check the provider's status page (https://status.hetzner.com/ for \
               hetzner-cloud).\n\
             • If you're behind a VPN / corporate proxy, ensure `https://api.hetzner.cloud/` is \
               reachable.\n\
             • Pass `--no-ping` to skip the round-trip and save the target offline; you can \
               re-verify later with `apprafter doctor`."
        )
    )]
    ProviderApiUnreachable {
        provider: String,
        #[source]
        #[diagnostic_source]
        cause: Box<dyn miette::Diagnostic + Send + Sync + 'static>,
    },

    /// Pass-through for `std::io::Error`.
    #[error("io error: {0}")]
    #[diagnostic(
        code(apprafter::io::error),
        help(
            "Low-level filesystem / network IO error. The captured OS message above usually \
             names the failing path or socket. Common cases: missing directory, wrong \
             permissions (`chmod 0600` on credentials), full disk, or a closed socket."
        )
    )]
    Io(#[from] io::Error),

    /// JSON encode/decode error.
    #[error("json error: {0}")]
    #[diagnostic(
        code(apprafter::io::json),
        help(
            "JSON decode/encode error — most often raised by state.json. If you hand-edited \
             that file or copied it across versions, delete it and re-run `apprafter import`."
        )
    )]
    Json(#[from] serde_json::Error),

    /// YAML encode/decode error (target store files use YAML).
    #[error("yaml error: {0}")]
    #[diagnostic(
        code(apprafter::io::yaml),
        help(
            "YAML decode/encode error — most often raised by a target store file under \
             `$XDG_CONFIG_HOME/apprafter/`. Re-create the offending target with \
             `apprafter target add <name> --force` to rewrite both halves \
             (`config.yaml` + `credentials.yaml`) cleanly."
        )
    )]
    Yaml(#[from] serde_yaml::Error),

    /// Catch-all, free-form message. New call sites should prefer
    /// promoting recurring messages to dedicated variants with
    /// stable diagnostic codes. The miette `code()` here remains
    /// constant so operators can still filter on it when grepping
    /// logs, but the help text is generic — variant-specific
    /// guidance lives on the typed variants.
    #[error("{0}")]
    #[diagnostic(
        code(apprafter::cli::other),
        help(
            "This is the catch-all CLI error. The message above is the only context. If you \
             see this often with the same wording, please file an issue — recurring messages \
             should be promoted to a typed `CliError` variant with its own help text."
        )
    )]
    Other(String),
}

/// `Result` alias used everywhere in the CLI crates.
pub type Result<T> = std::result::Result<T, CliError>;

#[cfg(test)]
mod tests {
    use super::*;
    use miette::Diagnostic;

    fn code_of(err: &CliError) -> String {
        err.code()
            .map(|c| format!("{c}"))
            .unwrap_or_else(|| "<no code>".to_string())
    }

    fn help_of(err: &CliError) -> String {
        err.help()
            .map(|h| format!("{h}"))
            .unwrap_or_else(|| "<no help>".to_string())
    }

    #[test]
    fn target_not_found_diagnostic_carries_stable_code_and_helpful_hint() {
        let err = CliError::TargetNotFound {
            name: "ghost".into(),
            available: "dev, work".into(),
        };
        assert_eq!(code_of(&err), "apprafter::target::not_found");
        let help = help_of(&err);
        // Help must steer the operator at the right next command.
        assert!(
            help.contains("apprafter target list"),
            "missing list hint: {help}"
        );
        assert!(
            help.contains("apprafter target add"),
            "missing add hint: {help}"
        );
    }

    #[test]
    fn invalid_target_config_diagnostic_points_at_target_directory() {
        let err = CliError::InvalidTargetConfig {
            path: PathBuf::from("/tmp/cfg.yaml"),
            message: "missing field `provider`".into(),
        };
        assert_eq!(code_of(&err), "apprafter::target::invalid_config");
        let help = help_of(&err);
        // Help should walk the operator through a recoverable path
        // without nuking the entire target store.
        assert!(
            help.contains("$XDG_CONFIG_HOME/apprafter/targets/"),
            "missing per-target path hint: {help}"
        );
        assert!(
            help.contains("apprafter target add"),
            "missing recreate hint: {help}"
        );
    }

    #[test]
    fn hetzner_diagnostic_help_enumerates_401_403_429_5xx() {
        let err = CliError::Hetzner {
            endpoint: "GET /v1/servers".into(),
            status: 401,
            code: "unauthorized".into(),
            message: "Invalid API token".into(),
        };
        assert_eq!(code_of(&err), "apprafter::provider::hetzner_api_error");
        let help = help_of(&err);
        // The hint must cover the 4 most common Hetzner failures
        // operators hit in the wild so the user can self-diagnose.
        for token in [
            "401",
            "403",
            "429",
            "5xx",
            "apprafter target add",
            "apprafter doctor",
        ] {
            assert!(help.contains(token), "missing `{token}` in help: {help}");
        }
    }

    #[test]
    fn server_type_unavailable_diagnostic_explains_retirement_path() {
        let err = CliError::ServerTypeUnavailable {
            requested: "cx22".into(),
            location: "nbg1".into(),
            reason: "server type was retired".into(),
            alternatives: "cpx22, cpx21".into(),
        };
        assert_eq!(
            code_of(&err),
            "apprafter::provider::server_type_unavailable"
        );
        let help = help_of(&err);
        // The retirement story (cx22 → cpx22 in early 2026) is the
        // single most common reason operators hit this — keep it
        // explicit so the error speaks for itself.
        assert!(help.contains("cx22"), "missing cx22 context: {help}");
        assert!(help.contains("cpx22"), "missing cpx22 context: {help}");
    }

    #[test]
    fn cue_not_found_diagnostic_recommends_nix_develop() {
        let err = CliError::CueNotFound;
        assert_eq!(code_of(&err), "apprafter::env::cue_not_found");
        let help = help_of(&err);
        assert!(help.contains("nix develop"), "missing nix hint: {help}");
        assert!(
            help.contains("docs/contributing/setup.md"),
            "missing setup-doc hint: {help}"
        );
    }

    #[test]
    fn invalid_state_diagnostic_recommends_import_for_recovery() {
        let err = CliError::InvalidState {
            path: PathBuf::from(".apprafter/state.json"),
            message: "expected `{`, found `[` at line 1".into(),
        };
        assert_eq!(code_of(&err), "apprafter::state::corrupt");
        let help = help_of(&err);
        // `apprafter import` is the safe escape hatch since it
        // rebuilds state from live Hetzner labels.
        assert!(
            help.contains("apprafter import"),
            "missing import hint: {help}"
        );
    }

    #[test]
    fn io_error_passes_through_with_dedicated_code() {
        let underlying = io::Error::new(io::ErrorKind::PermissionDenied, "perm denied");
        let err: CliError = underlying.into();
        assert_eq!(code_of(&err), "apprafter::io::error");
        // The wrapped OS message must survive into the rendered
        // `Display`, so operators can grep for the filename.
        assert!(format!("{err}").contains("perm denied"));
    }

    #[test]
    fn provider_token_rejected_carries_rotation_hint_and_chains_cause() {
        // Build the inner Hetzner variant first so we can chain it
        // through the typed wrapper, then verify miette can walk
        // the cause via the `#[diagnostic_source]` field.
        let inner = CliError::Hetzner {
            endpoint: "GET /v1/locations".into(),
            status: 401,
            code: "unauthorized".into(),
            message: "invalid token".into(),
        };
        let err = CliError::ProviderTokenRejected {
            provider: "hetzner-cloud".into(),
            cause: Box::new(inner),
        };
        assert_eq!(code_of(&err), "apprafter::target::token_rejected");
        let help = help_of(&err);
        // Help leads with rotation guidance, not generic API blame.
        assert!(
            help.contains("--renew --token"),
            "missing rotation hint: {help}"
        );
        assert!(
            help.contains("--no-ping"),
            "missing offline-fallback hint: {help}"
        );
        // The diagnostic source chain reaches the inner Hetzner
        // variant — miette walks this when rendering.
        let source = miette::Diagnostic::diagnostic_source(&err)
            .expect("token-rejected must expose its inner cause as a diagnostic_source");
        assert_eq!(
            source.code().map(|c| c.to_string()),
            Some("apprafter::provider::hetzner_api_error".to_string())
        );
    }

    #[test]
    fn provider_api_unreachable_targets_outage_path_not_rotation() {
        // Transport-error case (no HTTP status), wrapped as a
        // generic `Other`. The classifier treats this as
        // unreachable, not rejected.
        let inner = CliError::Other("connection refused".into());
        let err = CliError::ProviderApiUnreachable {
            provider: "hetzner-cloud".into(),
            cause: Box::new(inner),
        };
        assert_eq!(code_of(&err), "apprafter::target::provider_unreachable");
        let help = help_of(&err);
        // Help points operator at doctor + status page + --no-ping,
        // NOT at credential rotation.
        assert!(
            help.contains("apprafter doctor"),
            "missing doctor hint: {help}"
        );
        assert!(
            help.contains("status.hetzner.com"),
            "missing status-page hint: {help}"
        );
        assert!(
            help.contains("--no-ping"),
            "missing offline-fallback hint: {help}"
        );
        // Crucially, NOT a rotation problem — the rotation hint
        // belongs to `token_rejected`, surfacing it here would
        // misdirect operators.
        assert!(
            !help.contains("rotated / revoked"),
            "outage help should not mention rotation: {help}"
        );
    }

    #[test]
    fn other_keeps_catch_all_code_so_recurring_variants_can_be_filtered() {
        let err = CliError::Other("transient blip".into());
        // The catch-all has a stable code so log-analytics can spot
        // recurring messages and surface them as candidates for
        // promotion to a real variant.
        assert_eq!(code_of(&err), "apprafter::cli::other");
    }
}
