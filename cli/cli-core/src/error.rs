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
use std::path::{Path, PathBuf};

use miette::Diagnostic;
use thiserror::Error;

/// Classifies WHY a server type is unavailable, so callers can give
/// context-appropriate alternatives and help text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableKind {
    /// The SKU is not in the provider's catalog at all.
    Unknown,
    /// The SKU exists globally but is not offered in the requested region.
    NotOfferedInRegion,
    /// The SKU's `unavailable_after` timestamp has passed.
    Retired,
    /// The offer exists and is not retired, but capacity is currently exhausted.
    OutOfCapacity,
}

impl UnavailableKind {
    /// The snake_case name, for machine-readable surfaces (the desktop's error fields).
    pub fn as_str(self) -> &'static str {
        match self {
            UnavailableKind::Unknown => "unknown",
            UnavailableKind::NotOfferedInRegion => "not_offered_in_region",
            UnavailableKind::Retired => "retired",
            UnavailableKind::OutOfCapacity => "out_of_capacity",
        }
    }

    /// Why `requested` cannot be ordered in `location`, and what that leaves: the first line of
    /// `ServerTypeUnavailable`'s help. It names no command, so the desktop shows it as it is.
    pub fn why(self, requested: &str, location: &str) -> String {
        match self {
            UnavailableKind::Unknown => {
                format!("Hetzner sells no server type called `{requested}`; check its spelling.")
            }
            UnavailableKind::NotOfferedInRegion => format!(
                "`{requested}` is sold, but not in `{location}`: pick another region or another \
                 type."
            ),
            UnavailableKind::Retired => {
                format!("Hetzner no longer sells `{requested}`; pick another type.")
            }
            UnavailableKind::OutOfCapacity => format!(
                "`{requested}` is sold out in `{location}` right now; retry later or pick another \
                 type or region."
            ),
        }
    }

    /// Short human-readable reason clause used in the error `Display`.
    pub fn human_reason(self) -> &'static str {
        match self {
            UnavailableKind::Unknown => "unknown server type",
            UnavailableKind::NotOfferedInRegion => "not offered in this region",
            UnavailableKind::Retired => "retired (no longer orderable)",
            UnavailableKind::OutOfCapacity => {
                "out of capacity right now — try a different type or region, or retry later"
            }
        }
    }
}

/// Which command checked a server type: the way forward differs (bug 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkuCheckFor {
    /// `apply` / `up` (and `restore --reprovision`), about to create the machine.
    Provision,
    /// `target add <name> --server-type`.
    TargetAdd { name: String },
    /// `target machine` for target `name`.
    TargetMachine { name: String },
}

impl SkuCheckFor {
    /// The snake_case name, for machine-readable surfaces (the desktop's error fields).
    pub fn as_str(&self) -> &'static str {
        match self {
            SkuCheckFor::Provision => "provision",
            SkuCheckFor::TargetAdd { .. } => "target_add",
            SkuCheckFor::TargetMachine { .. } => "target_machine",
        }
    }
}

/// `ServerTypeUnavailable`'s help: why, per kind, then what to do, per command. It never points
/// at "the alternatives above", which can read "(no live alternatives found in this region)".
/// It names no server type of its own and no date (bug 10: "cx22 → cpx22 in early 2026" was shown for every
/// kind, beside alternatives that offered cx22), and the manifest key `spec.nodes[0].type`, not
/// the Rust field.
fn server_type_help(
    kind: &UnavailableKind,
    context: &SkuCheckFor,
    requested: &str,
    location: &str,
) -> String {
    let why = kind.why(requested, location);
    let what = match context {
        // The order is `apply`'s (`resolve_precedence`); `target machine` sits below the
        // manifest and the state, and refuses a target whose state records a server.
        SkuCheckFor::Provision => "`apprafter up` / `apprafter apply` take the server type \
             from the first of: `--server-type`, `spec.nodes[0].type` in the Infrastructure \
             manifest, the type recorded in the state at the last provision or import, the \
             target's (`apprafter target machine`), `APPRAFTER_SERVER_TYPE`; their `server \
             type:` line names the one used. Pass `--server-type <type>`, which overrides the \
             rest, or change that source: `apprafter target machine` takes effect only when \
             neither the manifest nor the state names a type."
            .to_string(),
        SkuCheckFor::TargetAdd { name } => format!(
            "Run the same `apprafter target add {name} …` again with another `--server-type \
             <type>`, or leave `--server-type` out and run `apprafter target machine --target \
             {name}` later. Nothing was saved."
        ),
        SkuCheckFor::TargetMachine { name } => format!(
            "Run `apprafter target machine --target {name} --server-type <type>` with another \
             type, or run it in a terminal without `--server-type` to open the picker. Nothing \
             was saved."
        ),
    };
    format!("{why}\n{what}")
}

/// `InvalidTargetConfig`'s help: the file, and the fixes that repair it. A target's own file
/// (`target`) can also be re-created by adding the target again, its directory deleted first
/// (`target remove` refuses a target it cannot read); the store's own `config.yaml` belongs to no
/// target, so no removal or re-add repairs it — it records only the default target, which
/// `target use` writes again once it is deleted (D.3d follow-up).
fn invalid_config_help(path: &Path, target: Option<&str>) -> String {
    let file = path.display();
    let why = "it was edited by hand or written by an incompatible CLI version. Fix it by hand \
               (it is a small YAML file), or restore it from a backup.";
    match target {
        Some(name) => {
            let dir = path.parent().unwrap_or(path).display();
            format!(
                "{file} could not be read as target `{name}`'s configuration: {why} Otherwise \
                 delete the target's directory, {dir}, and add the target again with \
                 `apprafter target add {name} --provider hetzner-cloud …` (its token too: the \
                 directory holds both its files)."
            )
        }
        None => format!(
            "{file} could not be read as the target store's configuration: {why} It records \
             only which target is the default, so deleting it and choosing the default again \
             with `apprafter target use <name>` works too."
        ),
    }
}

/// `SshKeyNotPublic`'s help: what a key must be, then the fix its source takes (D.3d
/// verification). `apply` takes the manifest's `sshKeys` first, then `APPRAFTER_SSH_PUBLIC_KEY`,
/// then the target's path, so each of the first two is fixed where it is; a file is pointed at
/// again — with `--renew` for an existing target, or the same `target add` for a new one.
fn ssh_key_not_public_help(source: &crate::ssh_key::KeySource) -> String {
    use crate::ssh_key::KeySource;
    let what =
        "AppRafter sends a target's SSH key to the provider, so it takes one OpenSSH public \
                key line, `<type> <base64> [comment]`, of type ssh-ed25519, ssh-rsa, an \
                ecdsa-sha2 curve, or a security-key (sk-) type.";
    let fix = match source {
        KeySource::TargetFile => "A private key's public half is the `.pub` file next to it \
             (`ssh-keygen -y -f <private key>` prints it again). Point the target at it with \
             `apprafter target add <name> --renew --ssh-key <path>.pub`, or set \
             `APPRAFTER_SSH_PUBLIC_KEY` to that line. Nothing was saved or sent."
            .to_string(),
        KeySource::NewTargetFile => "A private key's public half is the `.pub` file next to it \
             (`ssh-keygen -y -f <private key>` prints it again). Run the same `apprafter target \
             add` again with `--ssh-key <path>.pub`. Nothing was saved or sent."
            .to_string(),
        KeySource::Env => "`APPRAFTER_SSH_PUBLIC_KEY` holds the key's text, and it outranks \
             the target's key: set it to the public key line (`ssh-keygen -y -f <private key>` \
             prints it), or unset it so that the target's key is used. Nothing was sent."
            .to_string(),
        KeySource::Manifest { index } => format!(
            "The Infrastructure manifest's `sshKeys[{index}].public_key` holds the key's text: \
             replace it with the public key line (`ssh-keygen -y -f <private key>` prints it). \
             The manifest's keys outrank `APPRAFTER_SSH_PUBLIC_KEY` and the target's key, so \
             changing either of those does not change what is sent. Nothing was sent."
        ),
    };
    format!("{what} {fix}")
}

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

    /// A required external binary is not on `PATH`.
    ///
    /// Raised by [`crate::tools::preflight_tool`] BEFORE any prompt, any
    /// cluster round-trip and any billable provider call — see D11 in
    /// `docs/measurements/day2-followups.md` for the eight places where
    /// that ordering was inverted, the worst of which spent a paid
    /// Hetzner cluster before discovering the binary was missing.
    #[error("`{tool}` is required by `{needed_by}` ({purpose}) but is not on PATH")]
    #[diagnostic(
        code(apprafter::env::tool_not_found),
        help(
            "{install}\n\n\
             Nothing was sent anywhere and no credential was used: this check runs \
             before the command does any work, so re-running it after installing is \
             safe. `apprafter doctor` lists every external tool the CLI needs."
        )
    )]
    ExternalToolNotFound {
        tool: String,
        needed_by: String,
        purpose: String,
        install: String,
    },

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

    /// A `restic` invocation failed, classified.
    ///
    /// The whole point over the catch-all is `hint`: it comes from
    /// [`crate::diagnose::classify_restic`], so a wrong passphrase and a
    /// missing repository stop rendering as the same wall of stderr. An
    /// unrecognised failure carries an empty hint and the original text
    /// is all the reader gets — deliberately, since a confident wrong
    /// classification is worse than none.
    #[error("restic {verb} failed (exit {exit:?}): {stderr}")]
    #[diagnostic(code(apprafter::backup::restic_failed), help("{hint}"))]
    Restic {
        verb: String,
        exit: Option<i32>,
        stderr: String,
        hint: String,
    },

    /// `backup enable`'s repository probe failed: neither `restic cat
    /// config` (open an existing repo) nor `restic init` (create one)
    /// succeeded.
    ///
    /// Both stderrs are carried because they answer different questions
    /// — whether a repository is there, and what stopped one from being
    /// made — and `hint` classifies whichever of the two holds the
    /// diagnosis. The catch-all this replaced announced "unreachable /
    /// bad credentials" for every outcome, including the two that recur
    /// most: a bucket wedged by a previous failed `enable`, and a key
    /// that may read but not write.
    #[error(
        "backup repo '{repo}' could not be opened or created — `restic cat config` and \
         `restic init` both failed.\n  cat config stderr: {cat_stderr}\n  init stderr: {init_stderr}"
    )]
    #[diagnostic(code(apprafter::backup::repo_probe_failed), help("{hint}"))]
    BackupRepoProbe {
        repo: String,
        cat_stderr: String,
        init_stderr: String,
        hint: String,
    },

    /// `backup run` gave up on a Job whose pod no node would take.
    ///
    /// Typed rather than [`CliError::Other`] because the cause is known
    /// and so is the next step. The catch-all's help asks the reader to
    /// file an issue about recurring wording, which is the wrong advice
    /// here. The scheduler's reason, the runner's requests and the
    /// documentation link are printed on stdout just before this error,
    /// one per line. Inside the diagnostic, miette would wrap them to the
    /// terminal width and split the URL.
    ///
    /// `what` and `help` are written by the caller, because both depend on
    /// what the scheduler said and on what came before: only a lack of room
    /// is answered by freeing memory, a node's pressure taint lifts by
    /// itself, and an attempt after one that ran and failed did start.
    /// `backup enable` adds that the configuration is applied.
    #[error("backup Job {job} {what}")]
    #[diagnostic(code(apprafter::backup::runner_unschedulable), help("{help}"))]
    BackupRunnerUnschedulable {
        job: String,
        what: String,
        help: String,
    },

    /// `backup run` found a backup or check Job that has not finished, and
    /// started nothing beside it.
    ///
    /// Two runs at once do not both finish: two backups need the same helper
    /// pods, and a backup and a check each fail on the other's repository
    /// lock (neither waits for one). On a node with room for one runner, the
    /// second would not even be scheduled. What the Job is doing (`Running`,
    /// `Pending, cannot be scheduled: …`) is printed on stdout just before.
    #[error("Job {job} has not finished, so no second run was started beside it")]
    #[diagnostic(
        code(apprafter::backup::job_active),
        help(
            "The lines above say what {job} is doing. `apprafter backup status` shows when it \
             has finished; run `apprafter backup run` then. Two runs at once do not both \
             finish: two backups need the same helper pods, and a backup and a check each fail \
             on the other's repository lock."
        )
    )]
    BackupJobActive { job: String },

    /// A `kubectl` invocation failed, classified.
    ///
    /// Same shape as [`CliError::Restic`]: `hint` is derived from
    /// [`crate::diagnose::classify_kubectl`], which separates the three
    /// shapes that actually recur — cannot reach the apiserver, refused
    /// by RBAC, kind not served — because each has a different remedy
    /// and today they render identically.
    #[error("kubectl {verb} {resource} failed (exit {exit:?}): {stderr}")]
    #[diagnostic(code(apprafter::cluster::kubectl_failed), help("{hint}"))]
    Kubectl {
        verb: String,
        resource: String,
        exit: Option<i32>,
        stderr: String,
        hint: String,
    },

    /// Hetzner Cloud API call failed.
    #[error("hetzner-cloud {endpoint} failed (status {status}): {code}: {message}")]
    #[diagnostic(
        code(apprafter::provider::hetzner_api_error),
        // Each status with only what can cause it (D.3d follow-up). The token of a 401 need not
        // be a stored one: `target add` pings with the one it was given, and an `HCLOUD_TOKEN`
        // outranks the stored token.
        help(
            "The Hetzner Cloud API refused the request:\n\
             • 401 unauthorized — the token is wrong, or it was revoked or rotated. For a \
               target's stored token, `apprafter target add <name> --renew --token <new>` \
               replaces it; an `HCLOUD_TOKEN` in the environment outranks the stored token.\n\
             • 403 forbidden — the token lacks Read & Write (it is Read-only), or the project \
               forbids the call, for example at one of its limits.\n\
             • 429 rate limit — too many requests: wait, then try again.\n\
             • 5xx — an outage at the provider; check https://status.hetzner.com/.\n\
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
    /// same region (may contain newlines for multi-axis suggestions).
    /// The help depends on why (`kind`) and on which command checked
    /// (`context`), see [`server_type_help`].
    #[error(
        "server type `{requested}` is unavailable in region `{location}`: {}\n  {alternatives}",
        kind.human_reason()
    )]
    #[diagnostic(
        code(apprafter::provider::server_type_unavailable),
        help("{}", server_type_help(kind, context, requested, location))
    )]
    ServerTypeUnavailable {
        requested: String,
        location: String,
        /// Structured reason — replaces the old free-form `reason: String`.
        kind: UnavailableKind,
        /// Formatted suggestions (may contain newlines for multi-axis output).
        alternatives: String,
        /// The command that checked the type.
        context: SkuCheckFor,
    },

    /// No server type has been chosen yet.
    #[error("no server type selected")]
    #[diagnostic(
        code(apprafter::provider::server_type_not_selected),
        help(
            "No server type selected. Choose one:\n\
             • interactive: `apprafter target machine` (opens the machine picker)\n\
             • non-interactive / CI: `--server-type <sku>` or `APPRAFTER_SERVER_TYPE`\n\
             • declaratively: set `spec.nodes[0].type` in your Infrastructure manifest"
        )
    )]
    ServerTypeNotSelected,

    /// State file present but unparseable.
    #[error("state file at {path}: {message}")]
    #[diagnostic(
        code(apprafter::state::corrupt),
        help(
            "The state file named above failed to parse. It was written by a previous run \
             of `apprafter apply` / `import`. Run `apprafter import --force` to rebuild it \
             from live Hetzner resources tagged with `apprafter=true` — the unreadable file \
             is moved aside rather than deleted, so it stays available if you want to \
             salvage anything from it."
        )
    )]
    InvalidState { path: PathBuf, message: String },

    /// Target config / credentials / global-config file present but
    /// unparseable. Distinct from `InvalidState` so error messages
    /// can point users at the target store instead of `apprafter init`.
    /// The help depends on whose file it is ([`invalid_config_help`]).
    #[error("target config at {path}: {message}")]
    #[diagnostic(
        code(apprafter::target::invalid_config),
        help("{}", invalid_config_help(path, target.as_deref()))
    )]
    InvalidTargetConfig {
        path: PathBuf,
        message: String,
        /// The target the file belongs to (`targets/<name>/config.yaml` or `credentials.yaml`);
        /// `None` for the store's own `config.yaml`, which no target's removal or re-add repairs.
        target: Option<String>,
    },

    /// A subcommand asked for target `name`, but the target store
    /// has no such target. `available` lists what *is* configured
    /// (may be empty, signalling first-run + `apprafter target add`
    /// is the right next step).
    ///
    /// The name arrives positionally (`target show ghost`), through
    /// `--target`, or from a CLI default naming a removed target, so
    /// the help names the target and none of those sources.
    #[error("target `{name}` not found (available: {available})")]
    #[diagnostic(
        code(apprafter::target::not_found),
        help(
            "No target named `{name}` is configured here. `apprafter target list` lists the \
             targets that are; `apprafter target add {name} --provider hetzner-cloud …` \
             creates it. An empty `available:` list means this store has no targets yet — \
             start with `apprafter target add`."
        )
    )]
    TargetNotFound {
        name: String,
        /// Comma-separated list of configured target names; the
        /// empty string `""` means "no targets configured yet".
        available: String,
    },

    /// No `--target` was given and the store has no active target.
    #[error(
        "no active target — run `apprafter target add <name> --provider hetzner-cloud …` first, or \
         supply `--target <name>` to point at a specific one"
    )]
    #[diagnostic(
        code(apprafter::target::no_active),
        help(
            "List what is configured with `apprafter target list`; pick one with \
             `apprafter target use <name>`, or create the first with `apprafter target add <name>`."
        )
    )]
    NoActiveTarget,

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
        // The check is a read (GET /v1/locations): a Read-only token passes it, and a token
        // with a trailing newline never reaches it (the format check refuses that first). No
        // `--no-ping`: it would save the token the provider just refused (D.3d follow-up).
        help(
            "The provider's read-only credential check returned 401 unauthorized: the token \
             was mistyped, or it was revoked or rotated, or its project was deleted.\n\
             • The Hetzner Cloud Console shows a token only once, when it is created: paste it \
               again from where you saved it, or create a new one in the project under \
               Security → API tokens (AppRafter needs Read & Write).\n\
             • If you're rotating, run `apprafter target add <name> --renew --token <new>` \
               instead of re-creating the target."
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
             `$XDG_CONFIG_HOME/apprafter/`. Fix the YAML by hand (target store files are \
             small), or delete that target's directory under \
             `$XDG_CONFIG_HOME/apprafter/targets/<name>/` and add it again with `apprafter \
             target add <name> --provider hetzner-cloud …`. `target add --force` cannot \
             rewrite it: it keeps the stored values, so it needs a readable config."
        )
    )]
    Yaml(#[from] serde_yaml::Error),

    /// `apprafter completion <shell> --install` could not work out
    /// where to write: the shell has no published destination, or the
    /// base directory it hangs off does not resolve.
    ///
    /// Typed rather than [`CliError::Other`] because it is a DECISION,
    /// not a surprise — this command refuses a guessed path on purpose
    /// — and the catch-all's help tells the reader to file an issue
    /// about recurring wording, which is advice for the opposite case.
    #[error("{0}")]
    #[diagnostic(
        code(apprafter::completion::install),
        help(
            "`apprafter completion <shell>` without `--install` always works: it prints the \
             script, and you redirect it wherever that shell reads completions from. \
             `--install` writes it for you only where the destination is known — bash, zsh \
             and fish."
        )
    )]
    CompletionInstall(String),

    /// A destructive command run where no one can be asked: `--yes` is the confirmation.
    ///
    /// Typed rather than [`CliError::Other`]: it is a decision, and the catch-all's "file an
    /// issue" is the wrong advice. `action` completes "pass `--yes` to confirm …", e.g.
    /// "removing target `prod`".
    #[error("non-interactive invocation: pass `--yes` to confirm {action} (refusing silent destruction)")]
    #[diagnostic(
        code(apprafter::cli::confirmation_required),
        help(
            "Run the command in a terminal to be asked first, or pass `--yes` when you are sure."
        )
    )]
    ConfirmationRequired { action: String },

    /// The command line cannot work as given: a required input is missing or two flags
    /// contradict each other. `help` says what would work. CLI-input policy only — a domain
    /// refusal is a typed `apprafter_core::CoreError`.
    #[error("{message}")]
    #[diagnostic(code(apprafter::cli::usage_refused), help("{help}"))]
    UsageRefused { message: String, help: String },

    /// A target's SSH key — the file at a path given or stored, `APPRAFTER_SSH_PUBLIC_KEY`, or
    /// a manifest's `sshKeys` entry — is not one OpenSSH public key line (GOTCHA-149). Refused
    /// before a path is saved and before anything is sent to the provider: the private key sits
    /// next to its `.pub`, one dropped suffix away, and would otherwise leave the machine.
    /// `origin` names the key as shown (`crate::ssh_key::file_origin` for a file).
    #[error(
        "{origin} is {}",
        if *private_key {
            "a private key: AppRafter never sends a private key to the provider"
        } else {
            "not an OpenSSH public key"
        }
    )]
    #[diagnostic(
        code(apprafter::target::ssh_key_not_public),
        help("{}", ssh_key_not_public_help(from))
    )]
    SshKeyNotPublic {
        origin: String,
        private_key: bool,
        /// Where the key came from, which decides what fixes it ([`ssh_key_not_public_help`]).
        /// Not `source`: thiserror takes a field of that name for the error's cause.
        from: crate::ssh_key::KeySource,
    },

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

    #[test]
    fn unavailable_kinds_have_snake_case_names() {
        use UnavailableKind::*;
        assert_eq!(
            [Unknown, NotOfferedInRegion, Retired, OutOfCapacity].map(UnavailableKind::as_str),
            [
                "unknown",
                "not_offered_in_region",
                "retired",
                "out_of_capacity"
            ]
        );
    }

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
    fn no_active_target_carries_its_own_code_and_a_next_step() {
        let err = CliError::NoActiveTarget;
        assert_eq!(code_of(&err), "apprafter::target::no_active");
        // The message is the one the catch-all carried before, byte for byte.
        assert_eq!(
            err.to_string(),
            "no active target — run `apprafter target add <name> --provider hetzner-cloud …` \
             first, or supply `--target <name>` to point at a specific one"
        );
        let help = help_of(&err);
        for hint in [
            "apprafter target list",
            "apprafter target use",
            "apprafter target add",
        ] {
            assert!(help.contains(hint), "missing `{hint}`: {help}");
        }
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

    /// Bug 3: the name reaches this error positionally (`show`, `use`, `remove`), through
    /// `--target`, or from a CLI default naming a target that is gone. The help names the
    /// target and blames none of those sources.
    #[test]
    fn target_not_found_help_names_the_target_and_never_blames_a_flag() {
        let err = CliError::TargetNotFound {
            name: "ghost".into(),
            available: "dev".into(),
        };
        let help = help_of(&err);
        assert!(help.contains("`ghost`"), "{help}");
        assert!(!help.contains("`--target` flag"), "{help}");
        assert!(
            help.contains("`apprafter target list`")
                && help.contains("`apprafter target add ghost --provider hetzner-cloud …`"),
            "{help}"
        );
    }

    /// D.3d follow-up: a target's own file names the file, says to fix it by hand or restore
    /// it, and — the directory holding only that target — offers its re-creation, by the
    /// directory the file is in (an `APPRAFTER_CONFIG_DIR` store is not under
    /// `$XDG_CONFIG_HOME`).
    #[test]
    fn an_unreadable_target_file_names_it_and_offers_re_adding_that_target() {
        let dir = PathBuf::from("/s/targets/prod");
        for file in ["config.yaml", "credentials.yaml"] {
            let err = CliError::InvalidTargetConfig {
                path: dir.join(file),
                message: "missing field `provider`".into(),
                target: Some("prod".into()),
            };
            assert_eq!(code_of(&err), "apprafter::target::invalid_config");
            let help = help_of(&err);
            assert!(
                help.contains(&dir.join(file).display().to_string()),
                "{help}"
            );
            assert!(
                help.contains("by hand") && help.contains("restore"),
                "{help}"
            );
            assert!(help.contains(&dir.display().to_string()), "{help}");
            assert!(
                help.contains("`apprafter target add prod --provider hetzner-cloud …`"),
                "{help}"
            );
            assert!(!help.contains("$XDG_CONFIG_HOME"), "{help}");
        }
    }

    /// D.3d follow-up: no removal or re-add repairs the store's own `config.yaml`. Its help
    /// names it, says to fix it by hand or restore it, and — it holds only the default target —
    /// that deleting it and choosing the default again does too.
    #[test]
    fn an_unreadable_store_config_is_fixed_by_hand_restored_or_chosen_again() {
        let err = CliError::InvalidTargetConfig {
            path: PathBuf::from("/s/config.yaml"),
            message: "missing field `version`".into(),
            target: None,
        };
        assert_eq!(code_of(&err), "apprafter::target::invalid_config");
        let help = help_of(&err);
        assert!(help.contains("/s/config.yaml"), "{help}");
        assert!(
            help.contains("by hand") && help.contains("restore"),
            "{help}"
        );
        assert!(help.contains("`apprafter target use <name>`"), "{help}");
        for not_this in ["target add", "targets/", "remove"] {
            assert!(!help.contains(not_this), "{not_this}: {help}");
        }
    }

    /// D.3d verification (finding C): what fixes a refused SSH key depends on where it came
    /// from. The manifest's `sshKeys` outrank `APPRAFTER_SSH_PUBLIC_KEY`, which outranks the
    /// target's key, so pointing the target at another key fixes neither of the first two; a
    /// first `target add` has no target to `--renew`.
    #[test]
    fn the_ssh_key_help_offers_the_fix_its_source_takes() {
        use crate::ssh_key::{KeySource, NotAPublicKey};
        let help = |source: KeySource| {
            let err = NotAPublicKey::PrivateKey.refusal("the key", source);
            assert_eq!(code_of(&err), "apprafter::target::ssh_key_not_public");
            help_of(&err)
        };
        let existing = help(KeySource::TargetFile);
        for fix in ["--renew --ssh-key <path>.pub", "`.pub` file next to it"] {
            assert!(existing.contains(fix), "{fix}: {existing}");
        }
        let first = help(KeySource::NewTargetFile);
        assert!(first.contains("`--ssh-key <path>.pub`"), "{first}");
        assert!(first.contains("`.pub` file next to it"), "{first}");
        for wrong in ["--renew", "APPRAFTER_SSH_PUBLIC_KEY"] {
            assert!(!first.contains(wrong), "{wrong}: {first}");
        }
        let env = help(KeySource::Env);
        for fix in ["`APPRAFTER_SSH_PUBLIC_KEY`", "unset it", "ssh-keygen -y -f"] {
            assert!(env.contains(fix), "{fix}: {env}");
        }
        for wrong in ["--renew", "target add", "file next to it"] {
            assert!(!env.contains(wrong), "{wrong}: {env}");
        }
        let manifest = help(KeySource::Manifest { index: 3 });
        for fix in ["`sshKeys[3].public_key`", "ssh-keygen -y -f", "outrank"] {
            assert!(manifest.contains(fix), "{fix}: {manifest}");
        }
        for wrong in ["--renew", "target add", "file next to it", "set `APPRAFTER"] {
            assert!(!manifest.contains(wrong), "{wrong}: {manifest}");
        }
    }

    /// Bug 8: `--force` now keeps the stored values, so it refuses an unreadable target (it
    /// always did since it read both files) and cannot be the way to rewrite one.
    #[test]
    fn yaml_help_never_sends_the_reader_to_force() {
        let err = CliError::from(serde_yaml::from_str::<u8>("[").unwrap_err());
        assert_eq!(code_of(&err), "apprafter::io::yaml");
        let help = help_of(&err);
        assert!(!help.contains("<name> --force"), "{help}");
        assert!(help.contains("by hand"), "{help}");
        assert!(help.contains("targets/<name>/"), "{help}");
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
        // D.3d follow-up: each status with only what can cause it. 401 is the token (wrong,
        // revoked, rotated), whoever stored it; 403 is its permission or the project's; 429 is
        // waiting, never a quota (a 403 matter).
        let line = |status: &str| {
            help.lines()
                .find(|l| l.contains(status))
                .unwrap_or_else(|| panic!("{status}: {help}"))
                .to_string()
        };
        for why in ["wrong", "revoked", "rotated", "--renew --token"] {
            assert!(line("401").contains(why), "{why}: {help}");
        }
        assert!(!line("401").contains("stored API token was"), "{help}");
        assert!(line("403").contains("Read & Write"), "{help}");
        assert!(line("429").contains("wait"), "{help}");
        assert!(!help.contains("quota"), "{help}");
    }

    fn unavailable(kind: UnavailableKind, context: SkuCheckFor) -> CliError {
        CliError::ServerTypeUnavailable {
            requested: "cx99".into(),
            location: "nbg1".into(),
            kind,
            alternatives: "try one of: cx22".into(),
            context,
        }
    }

    const KINDS: [UnavailableKind; 4] = [
        UnavailableKind::Unknown,
        UnavailableKind::NotOfferedInRegion,
        UnavailableKind::Retired,
        UnavailableKind::OutOfCapacity,
    ];

    fn contexts() -> [SkuCheckFor; 3] {
        [
            SkuCheckFor::Provision,
            SkuCheckFor::TargetAdd { name: "p".into() },
            SkuCheckFor::TargetMachine { name: "p".into() },
        ]
    }

    /// Bugs 6 + 10: the remedy fits the command that checked the type, and the text names no
    /// SKU, no year, no Rust field and no flag that "will land".
    #[test]
    fn the_server_type_help_fits_the_command_and_names_no_sku_or_year() {
        let add = help_of(&unavailable(
            UnavailableKind::Unknown,
            SkuCheckFor::TargetAdd {
                name: "prod".into(),
            },
        ));
        assert!(
            add.contains("`apprafter target add prod …` again with another `--server-type <type>`"),
            "{add}"
        );
        let machine = help_of(&unavailable(
            UnavailableKind::Retired,
            SkuCheckFor::TargetMachine {
                name: "prod".into(),
            },
        ));
        assert!(
            machine.contains("apprafter target machine --target prod --server-type"),
            "{machine}"
        );
        let up = help_of(&unavailable(
            UnavailableKind::OutOfCapacity,
            SkuCheckFor::Provision,
        ));
        assert!(up.contains("spec.nodes[0].type"), "{up}");
        for kind in KINDS {
            for ctx in contexts() {
                let e = unavailable(kind, ctx);
                assert_eq!(code_of(&e), "apprafter::provider::server_type_unavailable");
                let (h, d) = (help_of(&e), e.to_string());
                for bad in ["cpx22", "2026", "nodes[0].kind", "once it lands"] {
                    assert!(!h.contains(bad) && !d.contains(bad), "{bad} in {d} / {h}");
                }
            }
        }
    }

    /// `apply` / `up` resolve the server type as `--server-type` > `spec.nodes[0].type` > the
    /// state's recorded type > the target's > `APPRAFTER_SERVER_TYPE`
    /// ([`crate::resolve::resolve_precedence`]): the provisioning help lists the sources in
    /// that order, says the flag overrides the rest, and says `target machine` helps only when
    /// neither the manifest nor the state names a type — it is refused on a target whose state
    /// records a server, and a type it saves is outranked by both.
    #[test]
    fn the_provisioning_help_follows_applys_precedence() {
        let up = help_of(&unavailable(
            UnavailableKind::Retired,
            SkuCheckFor::Provision,
        ));
        let at = |needle: &str| {
            up.find(needle)
                .unwrap_or_else(|| panic!("{needle:?} missing: {up}"))
        };
        let order = [
            at("`--server-type`"),
            at("`spec.nodes[0].type`"),
            at("recorded in the state"),
            at("the target's (`apprafter target machine`)"),
            at("`APPRAFTER_SERVER_TYPE`"),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{up}");
        assert!(
            up.contains("`--server-type <type>`, which overrides the rest"),
            "{up}"
        );
        assert!(
            up.contains(
                "`apprafter target machine` takes effect only when neither the manifest nor the \
                 state names a type"
            ),
            "{up}"
        );
        // The order the help lists is the resolver's: each rung wins over every later one.
        let rungs = ["flag", "manifest", "state", "target", "env"];
        for first in 0..rungs.len() {
            let r = |i: usize| (i >= first).then_some(rungs[i]);
            assert_eq!(
                crate::resolve::resolve_precedence(r(0), r(1), r(2), r(3), r(4)).as_deref(),
                Some(rungs[first])
            );
        }
    }

    /// The first line of the help says why, per kind; the alternatives stay in the message.
    #[test]
    fn the_server_type_help_says_why_for_each_kind() {
        let why = |kind| help_of(&unavailable(kind, SkuCheckFor::Provision));
        assert!(why(UnavailableKind::Unknown).contains("no server type called `cx99`"));
        assert!(why(UnavailableKind::NotOfferedInRegion).contains("not in `nbg1`"));
        assert!(why(UnavailableKind::Retired).contains("no longer sells `cx99`"));
        assert!(why(UnavailableKind::OutOfCapacity).contains("sold out in `nbg1`"));
        let shown = unavailable(UnavailableKind::Retired, SkuCheckFor::Provision).to_string();
        assert!(shown.contains("try one of: cx22"), "{shown}");
        // The alternatives line can say none were found, so the help never relies on it.
        for kind in KINDS {
            assert!(!why(kind).contains("above"), "{}", why(kind));
        }
    }

    #[test]
    fn sku_check_contexts_have_snake_case_names() {
        assert_eq!(
            contexts().map(|c| c.as_str()),
            ["provision", "target_add", "target_machine"]
        );
    }

    #[test]
    fn server_type_not_selected_has_stable_code_and_actionable_help() {
        let err = CliError::ServerTypeNotSelected;
        assert_eq!(
            code_of(&err),
            "apprafter::provider::server_type_not_selected"
        );
        let help = help_of(&err);
        assert!(
            help.contains("apprafter target machine"),
            "missing interactive hint: {help}"
        );
        assert!(help.contains("--server-type"), "missing CI hint: {help}");
        // The manifest hint is asserted by
        // `server_type_help_names_the_manifest_key_not_the_rust_field`,
        // which also pins the negative. This assertion used to require
        // `nodes[0].kind` — the Rust field name — and so held the
        // defect in place rather than catching it: a test that pins the
        // wrong string is worse than no test, because it defends the
        // bug on every run.
        assert!(
            help.contains("Infrastructure manifest"),
            "missing manifest hint: {help}"
        );
    }

    #[test]
    fn unavailable_kind_human_reason_covers_all_variants() {
        assert!(UnavailableKind::Unknown.human_reason().contains("unknown"));
        assert!(UnavailableKind::NotOfferedInRegion
            .human_reason()
            .contains("not offered"));
        assert!(UnavailableKind::Retired.human_reason().contains("retired"));
        assert!(UnavailableKind::OutOfCapacity
            .human_reason()
            .contains("capacity"));
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
    fn tool_not_found_names_the_command_and_carries_the_install_hint() {
        // The two halves that make this variant worth having over the
        // catch-all: the reader learns which command is blocked, and
        // gets an install line rather than `os error 2`.
        let err = CliError::ExternalToolNotFound {
            tool: "restic".into(),
            needed_by: "apprafter backup list".into(),
            purpose: "backup and restore".into(),
            install: "brew install restic".into(),
        };
        assert_eq!(code_of(&err), "apprafter::env::tool_not_found");

        let rendered = err.to_string();
        assert!(rendered.contains("apprafter backup list"), "{rendered}");
        assert!(rendered.contains("backup and restore"), "{rendered}");

        let help = help_of(&err);
        // Proves the per-tool install hint is interpolated rather than
        // dropped — a static help would pass every other assertion here.
        assert!(help.contains("brew install restic"), "{help}");
        // The sentence that matters after a passphrase prompt.
        assert!(
            help.contains("no credential was used"),
            "missing the nothing-happened reassurance: {help}"
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
        // D.3d follow-up: the check is a read (GET /v1/locations) that a Read-only token
        // passes, and the format check refuses a token with a trailing newline before it, so
        // neither a token's scope nor a newline can be why it answered 401. The console shows
        // a token only once, so "copy it again" is no step; `--no-ping` would save the token
        // the provider just refused.
        for why in [
            "mistyped",
            "revoked",
            "rotated",
            "only once",
            "create a new one",
        ] {
            assert!(help.contains(why), "{why}: {help}");
        }
        for not_why in ["scope", "newline", "Copy the token again", "--no-ping"] {
            assert!(!help.contains(not_why), "{not_why}: {help}");
        }
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
    fn server_type_help_names_the_manifest_key_not_the_rust_field() {
        // `kind` is the Rust field name — `type` is a keyword, so the
        // struct renames it (cli-core/src/manifest.rs). The manifest
        // key an author actually writes is `spec.nodes[0].type`
        // (schemas/v1alpha1/infrastructure.cue). This help named the
        // Rust side for months, telling operators to set a field the
        // schema does not have; every other surface got it right, so it
        // was the one outlier.
        let help = help_of(&CliError::ServerTypeNotSelected);
        assert!(
            help.contains("spec.nodes[0].type"),
            "help must name the manifest key: {help}"
        );
        assert!(
            !help.contains("nodes[0].kind"),
            "help must not name the Rust field: {help}"
        );
    }

    #[test]
    fn corrupt_state_help_points_at_the_repair_that_exists() {
        // The old help said "delete `.apprafter/`", which stopped being
        // the state location in v0.1.154 — an operator following it
        // removed a legacy artefact and kept hitting the error. It is
        // also no longer the remedy: `import --force` moves the
        // unreadable file aside and rebuilds, so nothing has to be
        // deleted by hand at all.
        let err = CliError::InvalidState {
            path: std::path::PathBuf::from("/anywhere/state.json"),
            message: "expected value".into(),
        };
        let help = help_of(&err);
        assert!(
            help.contains("import --force"),
            "help must name the repair that exists: {help}"
        );
        assert!(
            !help.contains("delete `.apprafter/`"),
            "help must not send the operator at a path state left in v0.1.154: {help}"
        );
    }

    #[test]
    fn an_unschedulable_backup_runner_has_its_own_code_and_no_file_an_issue_help() {
        // What happened and what to do depend on the scheduler's reason
        // and on the command, so the caller writes both; the code is fixed.
        let err = CliError::BackupRunnerUnschedulable {
            job: "apprafter-backup-manual-20260923-170848".into(),
            what: "never started: no node had room for its pod for 2m 3s".into(),
            help: "`apprafter top` shows how much of each node is requested.".into(),
        };
        assert_eq!(code_of(&err), "apprafter::backup::runner_unschedulable");
        assert_eq!(
            err.to_string(),
            "backup Job apprafter-backup-manual-20260923-170848 never started: no node had room \
             for its pod for 2m 3s"
        );
        let help = help_of(&err);
        assert_eq!(
            help,
            "`apprafter top` shows how much of each node is requested."
        );
        assert!(
            !help.contains("file an issue"),
            "a known cause must not get the catch-all's advice: {help}"
        );
    }

    #[test]
    fn a_run_refused_beside_an_active_job_has_its_own_code_and_names_the_job() {
        let err = CliError::BackupJobActive {
            job: "apprafter-backup-check-29312350".into(),
        };
        assert_eq!(code_of(&err), "apprafter::backup::job_active");
        assert_eq!(
            err.to_string(),
            "Job apprafter-backup-check-29312350 has not finished, so no second run was started \
             beside it"
        );
        let help = help_of(&err);
        assert!(help.contains("`apprafter backup status`"), "{help}");
        assert!(help.contains("`apprafter backup run`"), "{help}");
        assert!(help.contains("do not both finish"), "{help}");
        assert!(!help.contains("file an issue"), "{help}");
    }

    #[test]
    fn a_confirmation_refusal_has_its_own_code_and_says_how_to_confirm() {
        let err = CliError::ConfirmationRequired {
            action: "removing target `prod`".into(),
        };
        assert_eq!(code_of(&err), "apprafter::cli::confirmation_required");
        assert_eq!(
            err.to_string(),
            "non-interactive invocation: pass `--yes` to confirm removing target `prod` \
             (refusing silent destruction)"
        );
        let help = help_of(&err);
        assert!(
            help.contains("`--yes`") && help.contains("terminal"),
            "{help}"
        );
        assert!(!help.contains("file an issue"), "{help}");
    }

    #[test]
    fn a_usage_refusal_carries_the_callers_message_and_help() {
        let err = CliError::UsageRefused {
            message: "`--provider` is required".into(),
            help: "Supported providers: hetzner-cloud.".into(),
        };
        assert_eq!(code_of(&err), "apprafter::cli::usage_refused");
        assert_eq!(err.to_string(), "`--provider` is required");
        assert_eq!(help_of(&err), "Supported providers: hetzner-cloud.");
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
