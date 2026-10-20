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

/// The help of [`CliError::StorePathIo`]: a file or folder of the store this account cannot read
/// (d4a-h N3: never [`IO_HELP`]'s `chmod 0600` on credentials, which is no help for a folder).
const STORE_PATH_IO_HELP: &str = "This account cannot read this file or folder of the target \
                                  store: most often its permissions, or those of a folder above \
                                  it, do not let it. `ls -ld` on each shows them; the account \
                                  that uses a target store must be able to read all of it.";

/// The help of an I/O error, [`CliError::Io`] and [`CliError::TargetFileIo`] alike.
const IO_HELP: &str = "Low-level filesystem / network IO error. The captured OS message above \
                       usually names the failing path or socket. Common cases: missing \
                       directory, wrong permissions (`chmod 0600` on credentials), full disk, \
                       or a closed socket.";

/// `InvalidTargetConfig`'s help: the file, and the fixes that repair it. A target's own file
/// (`target`) can also be re-created by removing the target and adding it again (`target remove`
/// takes a target it cannot read, WI-458); the removal deletes the target's local state too, the
/// only local record of its server, so the help says so, and how `import` rebuilds the record
/// (WI-458 review #0/#2). The store's own `config.yaml` belongs to no target, so no removal or
/// re-add repairs it — it records only the default target, which `target use` writes again once
/// it is deleted (D.3d follow-up).
fn invalid_config_help(path: &Path, target: Option<&str>) -> String {
    let file = path.display();
    let why = "it was edited by hand or written by an incompatible CLI version. Fix it by hand \
               (it is a small YAML file), or restore it from a backup.";
    match target {
        Some(name) => format!(
            "{file} could not be read as target `{name}`'s configuration: {why} Otherwise \
             remove the target with `apprafter target remove {name}`, which takes a target it \
             cannot read, and add it again with `apprafter target add {name} --provider \
             hetzner-cloud …`, its token too. The removal also deletes the target's local \
             state: the record of its server, the cached kubeconfig and the Argo CD password. \
             If a server is recorded, fix or restore the file first. After adding the target \
             again, `apprafter import --target {name}` rebuilds the record from the provider; \
             the kubeconfig and the password are fetched again on first use."
        ),
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

/// The help of `apprafter::secrets::age_key_missing` (WI-457): restore the key, or fetch the
/// kubeconfig again under a new one. `target` is whose cache it is; `None` names a placeholder
/// (the core's `AgeKeyMissing` carries no target). The CLI's help for the core's variant uses it
/// too, so both say the same. A cache whose state records the target store vault it was
/// encrypted to has [`age_key_missing_lost_vault_help`] instead.
pub fn age_key_missing_help(target: Option<&str>) -> String {
    let target = target.unwrap_or("<name>");
    format!(
        "Restore the age key file the cache was encrypted with, or set `APPRAFTER_AGE_KEY` to \
         where it is. If the key is lost, `apprafter kubeconfig --refresh --target {target}` \
         fetches the kubeconfig from the node again over SSH and caches it under a new key. A \
         new key cannot read anything cached under the lost one, so the command lists those \
         secrets and asks before it creates the key."
    )
}

/// d4a-h F13's residue: `age_key_missing`'s help when every secret the store caches records the
/// target store vault it was encrypted to (`vaults`, its store ids) — `lock on` re-sealed it
/// there, and that vault is gone, since the shared key is looked for only on a store in files.
/// No age key opens it, so the way back is that vault, or a new fetch for `target`: never
/// "restore the age key file". The words are `kubeconfig --refresh`'s for the same case.
pub fn age_key_missing_lost_vault_help(target: &str, vaults: &str) -> String {
    format!(
        "What this store caches was encrypted to the key of a target store vault that is no \
         longer here (store id {vaults}), not to an age key: no age key opens it. If you still \
         have that vault and its `store.yaml`, put them back and unlock. Otherwise `apprafter \
         kubeconfig --refresh --target {target}` fetches the kubeconfig from the node again over \
         SSH and caches it under a new key; it lists what the new key cannot read and asks first."
    )
}

/// The way back a stopped `up` or `restore --reprovision` names first (WI-457), before the
/// refetch: the age key file; or, when the caches record the target store vault they were
/// encrypted to (`lost_vault`, F13's residue), that vault — no age key opens them.
fn way_back(lost_vault: Option<&str>) -> String {
    match lost_vault {
        None => "restore the age key file the cache was encrypted with, or set \
                 `APPRAFTER_AGE_KEY` to where it is. If the key is lost,"
            .to_string(),
        Some(vaults) => format!(
            "put back the target store vault the cache was encrypted to (store id {vaults}) and \
             its `store.yaml`, and unlock: no age key opens that cache. If that vault is lost,"
        ),
    }
}

/// The command that provisions a server and caches its kubeconfig under the age key: `up` or
/// `restore --reprovision` (WI-457). An error that stops it names this command to run again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionedBy {
    /// `apprafter up` (alias `bootstrap-all`).
    Up,
    /// `apprafter restore --reprovision`.
    RestoreReprovision,
}

impl ProvisionedBy {
    /// The command as the help names it.
    fn command(self) -> &'static str {
        match self {
            ProvisionedBy::Up => "`apprafter up`",
            ProvisionedBy::RestoreReprovision => "`apprafter restore --reprovision`",
        }
    }

    /// How to run it again for `target`.
    fn again(self, target: &str) -> String {
        match self {
            ProvisionedBy::Up => format!("run `apprafter up --target {target}` again"),
            ProvisionedBy::RestoreReprovision => format!(
                "run the same `apprafter restore` command again (the same repository and \
                 options, with `--reprovision --target {target}`)"
            ),
        }
    }

    /// What it goes on with once the node answers.
    fn goes_on(self) -> &'static str {
        match self {
            ProvisionedBy::Up => "cluster-bootstrap",
            ProvisionedBy::RestoreReprovision => "cluster-bootstrap and the restore",
        }
    }
}

/// Where `up` or `restore --reprovision` stopped on a lost age key (WI-457). Both refuse to
/// replace it; what that leaves depends on whether the server was created yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionStop {
    /// At the check before provisioning: the key is missing and something in this config root
    /// is cached under it, so phase 2 would refuse. Nothing was created. `recover` is a target
    /// with something cached under the lost key, whose `kubeconfig --refresh` creates the new
    /// one.
    Before { by: ProvisionedBy, recover: String },
    /// In phase 2, after the server was created (review #6): the key was there at the check
    /// before provisioning and went missing during the run. The server exists and bills, and
    /// cluster-bootstrap did not run.
    After { by: ProvisionedBy },
}

/// [`CliError::AgeKeyMissing`]'s help: [`age_key_missing_help`] for `target`, or, for `up` or
/// `restore --reprovision`, what its stop leaves and how to go on.
///
/// - Before provisioning: nothing was created. The key back, or `kubeconfig --refresh` for a
///   target that has something cached under it, then the same command again.
/// - After provisioning: a server that bills, no cluster-bootstrap. The key back or
///   `kubeconfig --refresh` for this target, then the same command again, which finds the
///   server instead of creating another.
fn age_key_missing_help_for(
    target: &str,
    stopped: Option<&ProvisionStop>,
    lost_vault: Option<&str>,
) -> String {
    match (stopped, lost_vault) {
        (None, None) => age_key_missing_help(Some(target)),
        (None, Some(vaults)) => age_key_missing_lost_vault_help(target, vaults),
        (Some(ProvisionStop::Before { by, recover }), _) => format!(
            "{} stopped before provisioning, so nothing was created. It caches the new \
             kubeconfig under the age key, and it does not create a new key in place of a \
             missing one: a new key cannot read what target `{recover}` cached before. To go \
             on, {} `apprafter kubeconfig --refresh --target {recover}` fetches that target's \
             kubeconfig from its node over SSH and caches it under a new key; it lists what the \
             new key cannot read and asks first. Then {}.",
            by.command(),
            way_back(lost_vault),
            by.again(target)
        ),
        (Some(ProvisionStop::After { by }), _) => format!(
            "{} stopped after provisioning: the server of target `{target}` exists, and Hetzner \
             bills for it, but cluster-bootstrap did not run. To go on, {} `apprafter \
             kubeconfig --refresh --target {target}` fetches the kubeconfig from the node over \
             SSH and caches it under a new key; it lists what the new key cannot read and asks \
             first, and the node may need a few minutes after provisioning before it answers. \
             Then {}: it finds the server it provisioned instead of creating another, and goes \
             on with {}.",
            by.command(),
            way_back(lost_vault),
            by.again(target),
            by.goes_on()
        ),
    }
}

/// [`CliError::K3sNotReady`]'s help: what the stop leaves (a server that bills, no
/// cluster-bootstrap) and how to go on: `by` again, which finds the server instead of creating
/// another and waits for the node again.
fn k3s_not_ready_help(target: &str, by: ProvisionedBy) -> String {
    format!(
        "{} stopped after provisioning: the server of target `{target}` exists, and Hetzner \
         bills for it, but cluster-bootstrap did not run. The node may still be starting; the \
         last error says how far it got. To go on, {}: it finds the server it provisioned \
         instead of creating another, waits for the node again, and goes on with {}. \
         `apprafter doctor --target {target}` checks whether the node's SSH port accepts \
         connections.",
        by.command(),
        by.again(target),
        by.goes_on()
    )
}

/// The command that fetches `secret` of `target` again, as a sentence starts with it.
fn refetch(secret: &crate::secrets::CachedSecret, target: &str) -> String {
    use crate::secrets::CachedSecret;
    match secret {
        CachedSecret::Kubeconfig => format!(
            "`apprafter kubeconfig --refresh --target {target}` fetches it from the node again"
        ),
        CachedSecret::ArgocdPassword => format!(
            "`apprafter argocd-password --refresh --target {target}` fetches it from the cluster \
             again"
        ),
    }
}

/// `CacheUndecryptable`'s message: the shared key's file, or the store's own key in its vault
/// — and, for a cache whose record names a store key a key change of this store retired
/// (D.4b B86), that key change, said as the record's: the record is no proof (critic
/// round, finding 3).
fn cache_undecryptable_message(
    secret: &crate::secrets::CachedSecret,
    target: &str,
    path: &str,
    detail: &str,
    key: crate::secrets::CacheKeyKind,
    reason: crate::secrets::UndecryptableWhy,
) -> String {
    use crate::secrets::{CacheKeyKind, UndecryptableWhy};
    match (key, reason) {
        (CacheKeyKind::StoreVault, UndecryptableWhy::RetiredKey) => format!(
            "the {} cached for target `{target}` is recorded as sealed to the store key of an \
             earlier key change, which that change retired: this target store's own key, in \
             its vault {path}, does not decrypt it",
            secret.noun()
        ),
        (CacheKeyKind::SharedFile, _) => format!(
            "the {} cached for target `{target}` cannot be decrypted with the age key at {path}: \
             {detail}",
            secret.noun()
        ),
        (CacheKeyKind::StoreVault, _) => format!(
            "the {} cached for target `{target}` cannot be decrypted with this target store's \
             own key, in its vault {path}: {detail}",
            secret.noun()
        ),
    }
}

/// `CacheUndecryptable`'s help: the key it was cached under, or the command that fetches it
/// again under the key there is now, for `target`, whichever target is active. On a locked
/// store the store's own key is the only one, so only the fetch (D.4a T38; ADR 0068 §6) —
/// after the key change that retired the key it was sealed to, when that is why (B86).
fn cache_undecryptable_help(
    secret: &crate::secrets::CachedSecret,
    target: &str,
    key: crate::secrets::CacheKeyKind,
    reason: crate::secrets::UndecryptableWhy,
) -> String {
    use crate::secrets::{CacheKeyKind, UndecryptableWhy};
    let refetch = refetch(secret, target);
    match (key, reason) {
        (CacheKeyKind::StoreVault, UndecryptableWhy::RetiredKey) => format!(
            "A key change (`apprafter lock rekey` or `apprafter lock recovery`) replaced this \
             target store's own key, and this copy's record names the key it replaced: it was \
             cached before that change, or restored from a copy made before it — or the copy \
             is damaged. {refetch} and caches it under the store's key."
        ),
        (CacheKeyKind::SharedFile, _) => format!(
            "It was cached under a different age key (one that was lost and replaced), or the \
             cached copy is damaged. If you still have the key it was cached under, set \
             `APPRAFTER_AGE_KEY` to it. Otherwise {refetch} and caches it under the key at that \
             path."
        ),
        (CacheKeyKind::StoreVault, _) => format!(
            "It was not encrypted to this target store's own key — an older AppRafter wrote it \
             under the shared age key, or another program did — or the copy is damaged. \
             {refetch} and caches it under the store's key."
        ),
    }
}

/// Why a locked target store could not be opened where a command needed its secrets (D.4a
/// T61, T66, §3.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockedWhy {
    /// No unlock window reaches this process, and nothing can ask for the secret: no terminal
    /// (a script, an AI agent's tool), or an app that holds no key.
    NoTerminal,
    /// The prompt was shown, and nobody answered it within 5 minutes.
    TimedOut,
}

impl LockedWhy {
    /// Its name on the wire (`fields.reason`).
    pub fn as_str(self) -> &'static str {
        match self {
            LockedWhy::NoTerminal => "no_terminal",
            LockedWhy::TimedOut => "timed_out",
        }
    }
}

/// Where a target store's secrets are, as an error names it: the twin of the shared core's
/// `apprafter_core::secrets::SecretsMode` (which crosses IPC; cli-core cannot name the core's
/// types), mapped one to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretsModeKind {
    Files,
    Locking,
    Locked,
    Unlocking,
    /// A key change did not finish (D.4b B63).
    Rotating,
}

impl SecretsModeKind {
    /// Its name in `store.yaml` and on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            SecretsModeKind::Files => "files",
            SecretsModeKind::Locking => "locking",
            SecretsModeKind::Locked => "locked",
            SecretsModeKind::Unlocking => "unlocking",
            SecretsModeKind::Rotating => "rotating",
        }
    }
}

/// Which lock flow an interrupted run left a store in (D.4a T31, T37).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockPhase {
    /// `secrets: locking`: an earlier `lock on` did not finish.
    Locking,
    /// `secrets: unlocking`: an earlier `lock off` did not finish.
    Unlocking,
    /// `secrets: rotating`: an earlier `lock rekey` or `lock recovery` did not finish (D.4b
    /// B63).
    Rotating,
}

impl LockPhase {
    /// Its name on the wire (`fields.phase`).
    pub fn as_str(self) -> &'static str {
        match self {
            LockPhase::Locking => "locking",
            LockPhase::Unlocking => "unlocking",
            LockPhase::Rotating => "rotating",
        }
    }

    /// The command that left it.
    fn command(self) -> &'static str {
        match self {
            LockPhase::Locking => "lock on",
            LockPhase::Unlocking => "lock off",
            LockPhase::Rotating => "lock rekey` or `lock recovery",
        }
    }
}

/// What is wrong with a vault that cannot be read (D.4a §3.12). Damage is found only by a key
/// that opened it or by its structure; a missing or foreign vault is said so before any key
/// is asked for. Never "no such secret" (ADR 0068 Risks "Failures in the vault code").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultProblem {
    /// `store.yaml` says the store is locked, and there is no vault.
    Missing,
    /// The file does not start like a vault (a text-mode copy reads as this too).
    NotAVault,
    /// A format this build does not read.
    NewerFormat { version: u16 },
    /// It fails a check of its structure or of its authentication.
    Damaged { what: DamageKind },
    /// It names another target store than `store.yaml` does.
    OtherStore,
}

impl VaultProblem {
    /// Its name on the wire (`fields.problem`).
    pub fn as_str(&self) -> &'static str {
        match self {
            VaultProblem::Missing => "missing",
            VaultProblem::NotAVault => "not_a_vault",
            VaultProblem::NewerFormat { .. } => "newer_format",
            VaultProblem::Damaged { .. } => "damaged",
            VaultProblem::OtherStore => "other_store",
        }
    }
}

/// Which check a damaged vault failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamageKind {
    /// A length, a cap, an order or a field the format refuses.
    Malformed,
    /// A slot's check value matched, and its wrapped key did not open.
    SlotCorrupt,
    /// The key was right, and the payload failed its authentication.
    Tampered,
    /// The payload authenticated and holds what the format refuses (no age identity).
    PayloadInvalid,
}

impl DamageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DamageKind::Malformed => "malformed",
            DamageKind::SlotCorrupt => "slot_corrupt",
            DamageKind::Tampered => "tampered",
            DamageKind::PayloadInvalid => "payload_invalid",
        }
    }
}

/// Why a vault was not written; nothing was (D.4a §3.7, T27, critic W7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultWriteProblem {
    /// It would hold more than `limit` bytes: a vault the format would refuse to read is never
    /// written.
    TooLarge { limit: u64 },
    /// The filesystem cannot create a file without risking replacing one (no hard links:
    /// exFAT, FAT); `fs` names it where the OS says.
    NoReplaceUnsupported { fs: Option<String> },
    /// A value the vault cannot hold, refused before anything was written — the write's
    /// input, never a damaged vault.
    Refused { what: RefusedInput },
}

impl VaultWriteProblem {
    /// Its name on the wire (`fields.problem`).
    pub fn as_str(&self) -> &'static str {
        match self {
            VaultWriteProblem::TooLarge { .. } => "too_large",
            VaultWriteProblem::NoReplaceUnsupported { .. } => "no_replace_unsupported",
            VaultWriteProblem::Refused { .. } => "refused",
        }
    }
}

/// Which value a vault write refused (c1's `PayloadWhat::{TargetName, HetznerToken,
/// TargetExists}` from its setters): cli-core's twin, as for `SecretsModeKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedInput {
    /// A target name the vault's entries cannot be keyed by (the callers check names first).
    TargetName,
    /// A Hetzner token that is empty or longer than 16384 bytes.
    Token,
    /// The new target name already has entries in the vault.
    NameTaken,
}

impl RefusedInput {
    /// Its name on the wire (`fields.what`).
    pub fn as_str(self) -> &'static str {
        match self {
            RefusedInput::TargetName => "target_name",
            RefusedInput::Token => "token",
            RefusedInput::NameTaken => "name_taken",
        }
    }

    /// What the vault refused, for the message.
    fn reason(self) -> &'static str {
        match self {
            RefusedInput::TargetName => {
                "a target name is 1 to 64 ASCII letters, digits and `-`, not at either end"
            }
            RefusedInput::Token => "a Hetzner token is 1 to 16384 bytes",
            RefusedInput::NameTaken => "the new target name already has entries in the vault",
        }
    }
}

/// Why a slot of a locked store was refused, before any key was derived (D.4a T22, T63).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotRefusal {
    /// Key-derivation settings below this build's floor.
    KdfTooWeak,
    /// Settings beyond what this build computes (memory or time): refused before allocating.
    KdfOutOfRange,
    /// No slot of a kind this build opens with that secret (only newer kinds).
    NoUsableSlot,
    /// Removing it would leave the store without a recovery key (T63).
    LastRecoverySlot,
    /// Removing it would leave no slot this build's policy can open (T63's second rule, c1's
    /// `NoUsableSlotLeft`).
    NoUsableSlotLeft,
    /// A second lock password (D.4b B18): a store has at most one; `lock rekey` changes it.
    AlreadyHasPassword,
    /// An account-password slot where there is none: Windows (S3: Linux and macOS only).
    NotOnThisOs,
    /// A vault already holding 16 slots, the most its format allows.
    SlotsFull,
}

impl SlotRefusal {
    /// Its name on the wire (`fields.reason`).
    pub fn as_str(self) -> &'static str {
        match self {
            SlotRefusal::KdfTooWeak => "kdf_too_weak",
            SlotRefusal::KdfOutOfRange => "kdf_out_of_range",
            SlotRefusal::NoUsableSlot => "no_usable_slot",
            SlotRefusal::LastRecoverySlot => "last_recovery_slot",
            SlotRefusal::NoUsableSlotLeft => "no_usable_slot_left",
            SlotRefusal::AlreadyHasPassword => "already_has_password",
            SlotRefusal::NotOnThisOs => "not_on_this_os",
            SlotRefusal::SlotsFull => "slots_full",
        }
    }
}

/// Why the account password was not taken for a slot (D.4b B29, B74, B75, B88). Nothing was
/// written in any case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountPasswordRefusal {
    /// The system checked it and refused it: a wrong password, or a locked account.
    Wrong,
    /// The system answered without checking what was typed — a fingerprint reader, a module
    /// that lets everyone in, a second prompt (B11). d4b-d counts it toward the back-off when
    /// PAM reported a failure (a second prompt refused), not when PAM let it pass (F1).
    NotChecked,
    /// The account has no password the system can check (B75).
    NoPassword,
    /// This computer cannot check it, or cannot bind a slot to itself.
    Unavailable { what: AccountCheckUnavailable },
    /// A check failed recently and the back-off across runs holds the next one back for
    /// `wait_secs` seconds; `None`: another AppRafter process is checking now (B12, B74).
    Backoff { wait_secs: Option<u64> },
    /// One a prompt could never read as a password (O2's rules, d4b-b1).
    Unusable { why: AccountPasswordUnusable },
}

impl AccountPasswordRefusal {
    /// Its name on the wire (`fields.reason`).
    pub fn as_str(&self) -> &'static str {
        match self {
            AccountPasswordRefusal::Wrong => "wrong",
            AccountPasswordRefusal::NotChecked => "not_checked",
            AccountPasswordRefusal::NoPassword => "no_password",
            AccountPasswordRefusal::Unavailable { .. } => "unavailable",
            AccountPasswordRefusal::Backoff { .. } => "backoff",
            AccountPasswordRefusal::Unusable { .. } => "unusable",
        }
    }
}

/// Why this computer cannot check the account password, or cannot bind a slot to itself
/// (B88's reasons, and d4b-d's outcome table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountCheckUnavailable {
    NoLibpam,
    NoService,
    NoCheckRoute,
    NoAccount,
    /// Linux: AppRafter runs with no_new_privs, under which the system may refuse even the
    /// right password (WI-545).
    NoNewPrivs,
    NoMachineId,
    NoBackoffRecord,
    /// The system's check failed with `name` (its PAM constant, `PAM_SYSTEM_ERR`), giving
    /// no verdict: d4b-d's `CheckOutcome::SystemError`. Its wire name is `system_error`,
    /// never a string literal `pam_…`, which d4b-d's G-OS4 refuses outside the OS crates
    /// (critic round, finding 1).
    SystemError {
        name: String,
    },
}

impl AccountCheckUnavailable {
    /// Its name on the wire (`fields.unavailable`).
    pub fn as_str(&self) -> &'static str {
        match self {
            AccountCheckUnavailable::NoLibpam => "no_libpam",
            AccountCheckUnavailable::NoService => "no_service",
            AccountCheckUnavailable::NoCheckRoute => "no_check_route",
            AccountCheckUnavailable::NoAccount => "no_account",
            AccountCheckUnavailable::NoNewPrivs => "no_new_privs",
            AccountCheckUnavailable::NoMachineId => "no_machine_id",
            AccountCheckUnavailable::NoBackoffRecord => "no_backoff_record",
            AccountCheckUnavailable::SystemError { .. } => "system_error",
        }
    }
}

/// An account password a prompt could never read as one (O2's rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPasswordUnusable {
    TooLong {
        max: usize,
    },
    RecoveryKeyShape,
    /// It holds a NUL character, which no system check takes: d4b-d's
    /// `InputRefused::ContainsNul`, refused before PAM (critic round, finding 9).
    ContainsNul,
}

impl AccountPasswordUnusable {
    /// Its name on the wire (`fields.why`).
    pub fn as_str(self) -> &'static str {
        match self {
            AccountPasswordUnusable::TooLong { .. } => "too_long",
            AccountPasswordUnusable::RecoveryKeyShape => "recovery_key_shape",
            AccountPasswordUnusable::ContainsNul => "contains_nul",
        }
    }
}

/// What another account can do to a folder of the target store (WI-475): one of the two ways
/// it could replace the store's files there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreFolderProblem {
    /// Another account owns the folder.
    ForeignOwner,
    /// Other accounts may write to it.
    WritableByOthers,
}

impl StoreFolderProblem {
    /// Its name on the wire (`fields.problem`).
    pub fn as_str(self) -> &'static str {
        match self {
            StoreFolderProblem::ForeignOwner => "foreign_owner",
            StoreFolderProblem::WritableByOthers => "writable_by_others",
        }
    }
}

/// [`CliError::AgeKeyIdentityCount`]'s message: a file with no key line, or with `found` of them.
fn age_key_identity_count_message(path: &Path, found: usize) -> String {
    match found {
        0 => format!(
            "the age key file {} holds no key: every line in it is blank or a comment",
            path.display()
        ),
        n => format!(
            "the age key file {} has {n} lines that are neither blank nor a comment, and \
             AppRafter takes a file with exactly one: the key",
            path.display()
        ),
    }
}

fn store_not_private_message(path: &Path, problem: StoreFolderProblem) -> String {
    let who = match problem {
        StoreFolderProblem::ForeignOwner => format!(
            "{} belongs to another account, which could replace",
            path.display()
        ),
        StoreFolderProblem::WritableByOthers => format!(
            "other accounts can write to {}, so they could replace",
            path.display()
        ),
    };
    format!(
        "{who} the target store's files there: AppRafter neither uses nor writes this store's \
         secrets until that is fixed"
    )
}

/// The fix for a store folder another account owns or can write, in this platform's terms.
pub fn store_not_private_help(path: &Path, problem: StoreFolderProblem) -> String {
    let fix = match (problem, cfg!(windows)) {
        (StoreFolderProblem::ForeignOwner, false) => format!(
            "have an administrator make your account its owner (`sudo chown <your user> {}`)",
            path.display()
        ),
        (StoreFolderProblem::WritableByOthers, false) => format!(
            "remove the other accounts' write access: `chmod go-w {}`",
            path.display()
        ),
        (StoreFolderProblem::ForeignOwner, true) => "have an administrator make your account \
             its owner (the folder's Properties › Security › Advanced)"
            .to_string(),
        (StoreFolderProblem::WritableByOthers, true) => "remove the other accounts' write \
             access in the folder's Properties › Security"
            .to_string(),
    };
    format!(
        "Whoever can replace a file there can swap in a token, a kubeconfig or a vault of their \
         own and send your next deploy to a project or a cluster of their choosing. Move the \
         target store to a folder only you can write (set `APPRAFTER_CONFIG_DIR` to it), or {fix}. \
         `apprafter doctor` lists every folder and file concerned."
    )
}

fn store_locked_message(store: &Path, reason: LockedWhy) -> String {
    match reason {
        LockedWhy::NoTerminal => format!(
            "the target store at {} is locked, and there is no unlock window here",
            store.display()
        ),
        LockedWhy::TimedOut => format!(
            "the target store at {} is locked, and nobody answered the unlock prompt within 5 \
             minutes",
            store.display()
        ),
    }
}

/// `store_locked`'s help (D.4a T69, T70): a person unlocks in their own terminal; `up` and
/// `restore --reprovision` ask before they provision (T46), so nothing was created. Never the
/// agent playground: a refusal shown to an agent does not say where else to look.
fn store_locked_help(stopped_before: Option<ProvisionedBy>) -> String {
    match stopped_before {
        None => "A person must unlock it in their own terminal: run `apprafter unlock` there, \
                 then run this command from that terminal while the window is open. A command \
                 started without a terminal — a script, an AI agent's tool — cannot ask for the \
                 lock password."
            .to_string(),
        Some(by) => format!(
            "{0} stopped before provisioning, so nothing was created. A person must unlock the \
             target store in their own terminal (`apprafter unlock`), then run {0} again from \
             that terminal.",
            by.command()
        ),
    }
}

/// `override_refused`'s help (D.4a T35, T70): unset it, or give the work a store of its own.
fn override_refused_help(var: &str) -> String {
    let what = match var {
        crate::HCLOUD_TOKEN_ENV => "the token stored for the target",
        crate::secrets::AGE_KEY_ENV => "the store's own age key, in its vault",
        _ => "the secrets in the store's vault",
    };
    format!(
        "Unset {var} to use {what} (the command asks for the lock password at a terminal). To \
         use a value from the environment on purpose, use a target store of its own that is not \
         locked: `APPRAFTER_CONFIG_DIR=<dir>`."
    )
}

fn unlock_failed_message(store: &Path, attempts: u32) -> String {
    let tries = if attempts == 1 { "attempt" } else { "attempts" };
    format!(
        "{attempts} {tries} did not unlock the target store at {}",
        store.display()
    )
}

fn unlock_failed_help(stopped_before: Option<ProvisionedBy>) -> String {
    let lead = "Nothing was unlocked. The recovery key opens the store at the same prompt if the \
                lock password is lost.";
    match stopped_before {
        None => lead.to_string(),
        Some(by) => format!(
            "{lead} {} stopped before provisioning, so nothing was created: unlock in your \
             terminal and run it again there.",
            by.command()
        ),
    }
}

fn store_changed_message(store: &Path, prepared: SecretsModeKind, now: SecretsModeKind) -> String {
    if prepared == now {
        format!(
            "the vault of the target store at {} was replaced while this command ran; nothing \
             was written",
            store.display()
        )
    } else {
        format!(
            "the lock state of the target store at {} changed while this command ran ({} → {}); \
             nothing was written",
            store.display(),
            prepared.as_str(),
            now.as_str()
        )
    }
}

fn vault_unwritable_message(path: &Path, problem: &VaultWriteProblem) -> String {
    match problem {
        VaultWriteProblem::TooLarge { limit } => format!(
            "the vault {} would hold more than {limit} bytes, the most a vault may: nothing was \
             written",
            path.display()
        ),
        VaultWriteProblem::NoReplaceUnsupported { fs } => format!(
            "a vault cannot be created in {}{}: this filesystem cannot create a file without \
             risking replacing one, so nothing was written",
            path.display(),
            fs.as_deref().map(|f| format!(" ({f})")).unwrap_or_default()
        ),
        VaultWriteProblem::Refused { what } => format!(
            "the vault {} cannot hold this write ({}): nothing was written",
            path.display(),
            what.reason()
        ),
    }
}

fn vault_unwritable_help(problem: &VaultWriteProblem) -> String {
    match problem {
        VaultWriteProblem::TooLarge { .. } => "A vault holds the tokens of a few thousand \
             targets. Remove targets this store no longer needs (`apprafter target remove \
             <name>`), or keep some in a target store of their own (`APPRAFTER_CONFIG_DIR`)."
            .to_string(),
        VaultWriteProblem::NoReplaceUnsupported { .. } => "Keep the target store on a \
             filesystem with hard links (ext4, APFS, NTFS, btrfs, xfs): `APPRAFTER_CONFIG_DIR` \
             moves it."
            .to_string(),
        VaultWriteProblem::Refused { what } => match what {
            RefusedInput::Token => "A Hetzner API token is one line of text: check what was \
                 pasted, then run the command again. The vault is as it was."
                .to_string(),
            RefusedInput::TargetName => "Choose a target name of ASCII letters, digits and \
                 `-`. The vault is as it was."
                .to_string(),
            RefusedInput::NameTaken => "Remove the target that has that name first \
                 (`apprafter target remove <name>`), or choose another name. The vault is as \
                 it was."
                .to_string(),
        },
    }
}

fn vault_unreadable_message(path: &Path, problem: &VaultProblem, detail: &str) -> String {
    let p = path.display();
    match problem {
        VaultProblem::Missing => format!("the vault {p} is missing"),
        VaultProblem::NotAVault => format!("{p} is not an AppRafter vault: {detail}"),
        VaultProblem::NewerFormat { version } => format!(
            "the vault {p} is in format {version}, which this AppRafter does not read: {detail}"
        ),
        VaultProblem::Damaged { .. } => format!("the vault {p} is damaged: {detail}"),
        VaultProblem::OtherStore => {
            format!("the vault {p} belongs to another target store: {detail}")
        }
    }
}

/// `vault_unreadable`'s help. It names no command, so the desktop shows it too (the shared
/// core's `neutral_help`).
pub fn vault_unreadable_help(path: &Path, problem: &VaultProblem) -> String {
    let p = path.display();
    match problem {
        VaultProblem::Missing => format!(
            "store.yaml says this target store is locked, but its vault is gone, and its \
             secrets are nowhere else: restore {p} from a backup."
        ),
        VaultProblem::NotAVault => format!(
            "Restore {p} from a backup. Never delete it while store.yaml says the store is \
             locked: nothing else holds these secrets."
        ),
        VaultProblem::NewerFormat { .. } => {
            "Upgrade AppRafter to the version that wrote it.".to_string()
        }
        VaultProblem::Damaged { .. } => format!(
            "Its contents fail their integrity check. Restore {p} from a backup; never delete \
             it: nothing else holds these secrets."
        ),
        VaultProblem::OtherStore => "The vault and store.yaml name different target stores: \
             one of them was copied from another store. Restore both from the same backup."
            .to_string(),
    }
}

fn lock_incomplete_help(phase: LockPhase) -> String {
    match phase {
        // D.4b-e1a: the everyday secret is the lock password or, by default, the account
        // password; and a run this AppRafter cannot finish (d4b-c3's Deviation 10) has one way
        // out, `lock off` with the recovery key.
        LockPhase::Locking => "Run `apprafter lock on` again: it asks for the store's everyday \
             secret — its lock password or your account password — and the recovery key, and \
             finishes. Without one of them, `apprafter lock off` backs out of it with the other; \
             where `apprafter lock on` says this AppRafter cannot finish the run, `apprafter lock \
             off` backs it out with the recovery key. A new `apprafter lock on` then makes a new \
             recovery key. Until then commands that need the store's secrets refuse, because some \
             may be only in the vault."
            .to_string(),
        LockPhase::Unlocking => "Run `apprafter lock off` again: it asks for the lock password \
             or the recovery key and finishes. Until then commands that need the store's \
             secrets refuse, because some may be only in the vault."
            .to_string(),
        LockPhase::Rotating => "Run `apprafter lock rekey` or `apprafter lock recovery` again: \
             either asks for one of the store's secrets and finishes the key change: an earlier \
             one if its new vault was not written yet, else a new one or a way in it kept. \
             `apprafter lock off` returns the secrets to files instead. Until then commands that \
             need the store's secrets refuse, because some may be sealed to the new store key and \
             some to the old one."
            .to_string(),
    }
}

fn slot_refused_message(store: &Path, reason: SlotRefusal) -> String {
    let s = store.display();
    match reason {
        SlotRefusal::KdfTooWeak => format!(
            "the lock password of the target store at {s} is kept with key-derivation settings \
             weaker than this AppRafter accepts"
        ),
        SlotRefusal::KdfOutOfRange => format!(
            "the lock password of the target store at {s} is kept with key-derivation settings \
             beyond what this AppRafter computes"
        ),
        SlotRefusal::NoUsableSlot => {
            format!("the target store at {s} has no slot this AppRafter can open with that secret")
        }
        SlotRefusal::LastRecoverySlot => {
            format!("the recovery key slot of the target store at {s} is its last one")
        }
        SlotRefusal::NoUsableSlotLeft => format!(
            "removing that slot would leave the target store at {s} with no slot this \
             AppRafter can open"
        ),
        SlotRefusal::AlreadyHasPassword => {
            format!("the target store at {s} has a lock password already")
        }
        SlotRefusal::NotOnThisOs => format!(
            "an account-password slot opens a target store only on Linux and macOS, and this \
             computer runs Windows (the target store at {s})"
        ),
        SlotRefusal::SlotsFull => format!(
            "the vault of the target store at {s} holds 16 ways to open it, the most a vault may"
        ),
    }
}

fn slot_refused_help(reason: SlotRefusal) -> String {
    match reason {
        // D.4b B24 (T22): `lock rekey` asks for one of the store's secrets first — the
        // recovery key opens the store — and sets a new lock password under this build's
        // settings.
        SlotRefusal::KdfTooWeak | SlotRefusal::KdfOutOfRange => "Run `apprafter lock rekey` \
             and give the recovery key when it asks; it sets a new lock password."
            .to_string(),
        // B29, with the account-password slot's binding to its computer (B54).
        SlotRefusal::NoUsableSlot => "This AppRafter cannot open this store's other slots \
             here: a Windows Hello slot opens only on the computer that made it, an \
             account-password slot only on Linux and macOS and on the computer that made it, a \
             slot of a newer AppRafter only after an upgrade. Use the lock password or the \
             recovery key."
            .to_string(),
        SlotRefusal::LastRecoverySlot => "A locked store always keeps its recovery key: \
             `apprafter lock recovery` replaces it with a new one."
            .to_string(),
        SlotRefusal::NoUsableSlotLeft => "A locked store always keeps a slot this AppRafter \
             opens: add one before removing this one."
            .to_string(),
        SlotRefusal::AlreadyHasPassword => "A store has at most one lock password: `apprafter \
             lock rekey` changes it."
            .to_string(),
        SlotRefusal::NotOnThisOs => "On Windows, add a separate lock password instead: \
             `apprafter lock slot add password`."
            .to_string(),
        SlotRefusal::SlotsFull => "Remove a way to open it that it no longer needs first: \
             `apprafter lock slot list` shows them with their ids, and `apprafter lock slot \
             remove <id>` removes one."
            .to_string(),
    }
}

/// `slot_not_found`'s message: no slot, or the slots `given` could mean, by their short ids.
fn slot_not_found_message(store: &Path, given: &str, matches: &[String]) -> String {
    let s = store.display();
    if matches.is_empty() {
        return format!("the target store at {s} has no slot `{given}`");
    }
    let shown: Vec<&str> = matches.iter().map(|m| m.get(..8).unwrap_or(m)).collect();
    format!(
        "`{given}` could mean more than one slot of the target store at {s}: {}",
        shown.join(", ")
    )
}

/// `account_password_refused`'s message, then the system's own words when it gave any.
fn account_password_refused_message(
    reason: &AccountPasswordRefusal,
    messages: &[String],
) -> String {
    let base = match reason {
        AccountPasswordRefusal::Wrong => {
            "the system did not accept that account password".to_string()
        }
        AccountPasswordRefusal::NotChecked => {
            "the system did not check the password that was typed: a \
             fingerprint reader, another sign-in module or a second question took its place"
                .to_string()
        }
        AccountPasswordRefusal::NoPassword => {
            "this account has no password the system can check".to_string()
        }
        AccountPasswordRefusal::Unavailable { what } => match what {
            AccountCheckUnavailable::NoLibpam => {
                "this computer cannot check your account password: libpam is not installed"
                    .to_string()
            }
            AccountCheckUnavailable::NoService => "this computer cannot check your account \
                 password: none of the sign-in services AppRafter uses (common-auth, \
                 system-auth, login) is set up"
                .to_string(),
            AccountCheckUnavailable::NoCheckRoute => "this computer cannot check your account \
                 password: the system offers no check that sees what was typed"
                .to_string(),
            AccountCheckUnavailable::NoAccount => "this computer cannot check your account \
                 password: the user running AppRafter has no account entry"
                .to_string(),
            AccountCheckUnavailable::NoNewPrivs => "AppRafter cannot check your account \
                 password safely while it runs with no_new_privs (a sandbox such as Flatpak or \
                 bubblewrap, or a service started with NoNewPrivileges=): the system may refuse \
                 even the right password and count it as a failed login"
                .to_string(),
            AccountCheckUnavailable::NoMachineId => "this computer's machine id cannot be \
                 read, and an account-password slot is bound to it"
                .to_string(),
            AccountCheckUnavailable::NoBackoffRecord => "this computer cannot check your \
                 account password safely: there is no place for the record of failed checks \
                 (the per-user runtime folder is missing)"
                .to_string(),
            AccountCheckUnavailable::SystemError { name } => {
                format!("the system's password check failed ({name}) without a verdict")
            }
        },
        AccountPasswordRefusal::Backoff {
            wait_secs: Some(secs),
        } => format!(
            "an account-password check failed recently: the next is allowed in {} min",
            secs.div_ceil(60)
        ),
        AccountPasswordRefusal::Backoff { wait_secs: None } => {
            "another AppRafter process is checking an account password right now".to_string()
        }
        AccountPasswordRefusal::Unusable { why } => match why {
            AccountPasswordUnusable::TooLong { max } => {
                format!("that account password is longer than {max} bytes, which no slot holds")
            }
            AccountPasswordUnusable::RecoveryKeyShape => "that account password has the shape \
                 of a recovery key, and a prompt reads such text as a recovery key"
                .to_string(),
            AccountPasswordUnusable::ContainsNul => "that account password holds a NUL \
                 character, which the system's password check cannot take"
                .to_string(),
        },
    };
    if messages.is_empty() {
        base
    } else {
        format!("{base}; the system said: {}", messages.join("; "))
    }
}

/// The way out every account-password refusal shares: a separate lock password, which only
/// AppRafter's prompt sees.
const LOCK_PASSWORD_INSTEAD: &str = "a separate lock password: `apprafter lock on \
     --lock-password` for a new lock, `apprafter lock slot add password` on a locked store";

fn account_password_refused_help(reason: &AccountPasswordRefusal) -> String {
    match reason {
        AccountPasswordRefusal::Wrong => format!(
            "Nothing was written. A failed check may count as a failed login on systems that \
             lock accounts after failures, so AppRafter waits before it checks again: run the \
             command again later, or use {LOCK_PASSWORD_INSTEAD}. If this account has no \
             usable password — a locked one, a key-only login — use a lock password."
        ),
        AccountPasswordRefusal::NotChecked => format!(
            "Nothing was written. Type the password when the system asks for it — not a \
             fingerprint — or use {LOCK_PASSWORD_INSTEAD}."
        ),
        AccountPasswordRefusal::Backoff { .. } => format!(
            "AppRafter spaces out account-password checks after a failed one, so that typos \
             cannot lock your account: run the command again then, or use \
             {LOCK_PASSWORD_INSTEAD}."
        ),
        AccountPasswordRefusal::NoPassword
        | AccountPasswordRefusal::Unavailable { .. }
        | AccountPasswordRefusal::Unusable { .. } => format!("Use {LOCK_PASSWORD_INSTEAD}."),
    }
}

fn cache_not_sealed_help(secret: &crate::secrets::CachedSecret, target: &str) -> String {
    format!(
        "An older AppRafter or another program wrote it unencrypted into this locked store. {} \
         and caches it encrypted to the store's own key.",
        refetch(secret, target)
    )
}

/// A provider request that got no answer — the name did not resolve, the connection was
/// refused or dropped, the request timed out — as [`CliError::ProviderApiUnreachable`] carries
/// it. `detail` is the HTTP client's account without the URL, which `endpoint` already names.
/// No code and no help of its own: the variant carrying it has both, so a rendered chain shows
/// it as one line under them, not as a second diagnostic with the catch-all's "file an issue".
#[derive(Debug, Error, Diagnostic)]
#[error("transport error talking to {endpoint}: {detail}")]
pub struct TransportFailure {
    pub endpoint: String,
    pub detail: String,
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
               target's stored token, `apprafter target add <name> --renew` replaces it (at a \
               terminal it asks for the new token without showing it; in a script, \
               `--token`); an `HCLOUD_TOKEN` in the environment outranks the stored token.\n\
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

    /// The target stores no Hetzner token: the core's `TokenNotStored`, for a command that still
    /// returns `cli_core::Result` (D.4a T15) — its code, message and CLI help. The shared core's
    /// `CoreError::from` maps it back onto its own variant, so the core carries one shape.
    #[error("target `{name}` has no provider token stored")]
    #[diagnostic(
        code(apprafter::target::token_missing),
        help(
            "Run `apprafter target add {name} --renew` to store one (at a terminal it asks for \
             the new token without showing it; in a script, `--token`), or set `HCLOUD_TOKEN` \
             for this invocation."
        )
    )]
    TokenNotStored { name: String },

    /// The person stopped a prompt (Ctrl-C at the controlling terminal, D.4a T61): the core's
    /// `Cancelled`, for a command that still returns `cli_core::Result`. `CoreError::from` maps it
    /// back onto the core's own variant.
    #[error("operation cancelled")]
    #[diagnostic(
        code(apprafter::op::cancelled),
        help("The command was interrupted; nothing after that point ran.")
    )]
    Cancelled,

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
             • A token is shown only once, when it is created: paste it again from where you \
               saved it, or create a new one with Read & Write permission in {}.\n\
             • If you're rotating, run `apprafter target add <name> --renew` instead of \
               re-creating the target (at a terminal it asks for the new token without showing \
               it; in a script, `--token`).",
            crate::target::HETZNER_API_TOKENS_PAGE
        )
    )]
    ProviderTokenRejected {
        provider: String,
        #[source]
        #[diagnostic_source]
        cause: Box<dyn miette::Diagnostic + Send + Sync + 'static>,
    },

    /// The provider's API gave no answer: the Hetzner client raises it for every request that
    /// got none (a failed lookup, a refused or dropped connection, a timeout), the cause a
    /// [`TransportFailure`]. One code for a dead API, whichever request found it, the token
    /// check's included (WI-453); a provider that answered is never this — an error status is
    /// [`CliError::Hetzner`], a 429 or a 5xx among them. The help points at `apprafter doctor`,
    /// the status page and, for the commands that have it, `--no-ping`.
    #[error("provider `{provider}` API was unreachable")]
    #[diagnostic(
        code(apprafter::target::provider_unreachable),
        help(
            "The request could not complete because the provider's API was unreachable. This \
             is NOT a credentials problem — the token may still be valid once the API \
             recovers.\n\
             • Run `apprafter doctor` to confirm reachability + DNS.\n\
             • Check the provider's status page (https://status.hetzner.com/ for \
               hetzner-cloud).\n\
             • If you're behind a VPN / corporate proxy, ensure `https://api.hetzner.cloud/` is \
               reachable.\n\
             • Pass `--no-ping` to skip the provider round-trip and save offline (`target \
               add`, also with `--renew`, and `target machine` take it); you can re-verify \
               later with `apprafter doctor`."
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
    #[diagnostic(code(apprafter::io::error), help("{}", IO_HELP))]
    Io(#[from] io::Error),

    /// An I/O error on one target's own file, its `config.yaml` or `credentials.yaml`: reading
    /// it, or checking that it exists (a target directory this user cannot search). [`Io`]'s
    /// code, cause and help, plus whose file it is: the desktop offers that target's removal
    /// for it, as for a file that does not parse (WI-458 review #6). Its message names the file,
    /// which the OS message does not (WI-482).
    ///
    /// [`Io`]: CliError::Io
    #[error("io error: {}: {source}", path.display())]
    #[diagnostic(code(apprafter::io::error), help("{}", IO_HELP))]
    TargetFileIo {
        path: PathBuf,
        target: String,
        #[source]
        source: io::Error,
    },

    /// An I/O error on a file or folder of the target store's own — its root, `targets/`, an
    /// entry in it, the root's `config.yaml` — reading it, or checking that it exists (WI-482).
    /// [`Io`]'s code. The message names the path, which the OS message does not, and the OS
    /// message is its cause, shown once (d4a-h N3: as an [`Io`] carrying both, the CLI printed
    /// it twice, under a help about `chmod 0600` on credentials).
    ///
    /// [`Io`]: CliError::Io
    #[error("cannot read {}", path.display())]
    #[diagnostic(code(apprafter::io::error), help("{}", STORE_PATH_IO_HELP))]
    StorePathIo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

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

    /// YAML encode/decode error (target store files use YAML). The removal it offers deletes
    /// the target's local state too, so the help says so as `invalid_config`'s does (WI-458
    /// review #0/#2).
    #[error("yaml error: {0}")]
    #[diagnostic(
        code(apprafter::io::yaml),
        help(
            "YAML decode/encode error — most often raised by a target store file under \
             `$XDG_CONFIG_HOME/apprafter/`. Fix the YAML by hand (target store files are \
             small), or restore it from a backup. Otherwise remove that target with \
             `apprafter target remove <name>`, which takes a target it cannot read, and add it \
             again with `apprafter target add <name> --provider hetzner-cloud …`. The removal \
             also deletes the target's local state: the record of its server, the cached \
             kubeconfig and the Argo CD password. If a server is recorded, fix or restore the \
             file first. After adding the target again, `apprafter import --target <name>` \
             rebuilds the record from the provider; the kubeconfig and the password are \
             fetched again on first use. `target add --force` cannot rewrite such a target: \
             it keeps the stored values, so it needs a readable config."
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

    /// The age key is absent where a cached secret needs it, and a read never creates one
    /// (GOTCHA-120): a new key opens nothing cached before it. The core's `AgeKeyMissing`, same
    /// code and message, as the CLI's own commands raise it, with the target whose cache it is
    /// for the help's way back; the core takes it as its own variant (one shape per code).
    #[error("no age key at {path}; the cached secrets cannot be decrypted")]
    #[diagnostic(
        code(apprafter::secrets::age_key_missing),
        help(
            "{}",
            age_key_missing_help_for(target, stopped.as_ref(), lost_vault.as_deref())
        )
    )]
    AgeKeyMissing {
        path: String,
        target: String,
        /// Set when `up` or `restore --reprovision` stopped on the key, before provisioning or
        /// after: the help says what that leaves and how to go on.
        stopped: Option<ProvisionStop>,
        /// d4a-h F13's residue: the store ids of the target store vaults the store's caches record
        /// they were encrypted to, when each one records one (`SealedTo::StoreVault`). That vault is
        /// gone — the shared key is looked for only on a store in files — so no age key opens
        /// them, and the help names the vault, never "restore the age key file".
        lost_vault: Option<String>,
    },

    /// An age key file that does not hold one key (WI-478). It is read as age-keygen writes it:
    /// a line that is blank or starts with `#` is skipped, and exactly one other line is the key
    /// (`AGE-SECRET-KEY-1…`). `found` is how many such lines it has: none, or more than one. The
    /// file is never used, and never replaced. A key line that does not parse stays the
    /// catch-all that names the file (WI-475's words).
    #[error("{}", age_key_identity_count_message(path, *found))]
    #[diagnostic(
        code(apprafter::secrets::age_key_identity_count),
        help(
            "An age key file holds one key, the line `AGE-SECRET-KEY-1…`; lines that start with \
             `#` and blank lines are skipped, as in the file `age-keygen -o <file>` writes. Keep \
             only the key the cached secrets were encrypted with in this file, or set \
             `APPRAFTER_AGE_KEY` to a file that holds only that key. The file was left as it is."
        )
    )]
    AgeKeyIdentityCount { path: PathBuf, found: usize },

    /// A cached secret the key it was tried with cannot decrypt: it was cached under another key,
    /// or the ciphertext is damaged. `detail` is the age library's own account; `key` says which
    /// key (the shared file at `path`, or the store's own, in its vault at `path`) and `reason`
    /// which failure (D.4a §3.12).
    #[error("{}", cache_undecryptable_message(secret, target, path, detail, *key, *reason))]
    #[diagnostic(
        code(apprafter::secrets::cache_undecryptable),
        help("{}", cache_undecryptable_help(secret, target, *key, *reason))
    )]
    CacheUndecryptable {
        secret: crate::secrets::CachedSecret,
        target: String,
        path: String,
        detail: String,
        key: crate::secrets::CacheKeyKind,
        reason: crate::secrets::UndecryptableWhy,
    },

    /// The user declined the new age key `apprafter kubeconfig` asks for when the key the cache
    /// was encrypted under is lost (WI-457): answered no, or pressed Esc or Ctrl-C at the
    /// question. Nothing was created, fetched or written. An error rather than a quiet exit: the
    /// command's stdout is the kubeconfig, and a caller capturing it (`apprafter kubeconfig >
    /// kc && …`) must not take an empty one for success.
    #[error(
        "no new age key was created at {path}, so the kubeconfig of target `{target}` was not \
         fetched"
    )]
    #[diagnostic(
        code(apprafter::secrets::new_key_declined),
        help(
            "Nothing was created, fetched or written. If you still have the lost key, put it \
             back, or set `APPRAFTER_AGE_KEY` to where it is. To go on with a new key, run \
             `apprafter kubeconfig --refresh --target {target}` again and agree, or pass `--yes` \
             where there is no terminal to ask at."
        )
    )]
    NewAgeKeyDeclined { path: String, target: String },

    /// `up` or `restore --reprovision` provisioned the server, and in phase 2 (`k3s-ready`)
    /// its kubeconfig did not become reachable within the wait: the node did not answer over
    /// SSH, k3s had not written the file, or the apiserver did not accept connections.
    /// `last_error` is the last of those. The help says what that leaves (a server that bills,
    /// no cluster-bootstrap) and that the same command, run again, goes on.
    #[error(
        "the kubeconfig of target `{target}` did not become reachable within {waited_secs}s \
         ({attempts} attempts); last error: {last_error}"
    )]
    #[diagnostic(
        code(apprafter::cluster::k3s_not_ready),
        help("{}", k3s_not_ready_help(target, *by))
    )]
    K3sNotReady {
        target: String,
        by: ProvisionedBy,
        waited_secs: u64,
        attempts: u32,
        last_error: String,
    },

    /// A locked target store whose secrets a command needs, and nothing that can open it: no
    /// unlock window and no terminal to ask at, or a prompt nobody answered (D.4a T61, T69).
    /// `stopped_before` is set when `up` or `restore --reprovision` stopped at their phase 0,
    /// before anything was provisioned (T46; never `ProvisionStop`, critic C2).
    #[error("{}", store_locked_message(store, *reason))]
    #[diagnostic(
        code(apprafter::secrets::store_locked),
        help("{}", store_locked_help(*stopped_before))
    )]
    StoreLocked {
        store: PathBuf,
        reason: LockedWhy,
        stopped_before: Option<ProvisionedBy>,
    },

    /// `HCLOUD_TOKEN` or `APPRAFTER_AGE_KEY` set for a command on a locked target store, which
    /// uses only the secrets in its vault (ADR 0068 §6; D.4a T35); refused before any prompt.
    #[error(
        "{var} is set, but the target store at {} is locked: a locked store uses only the \
         secrets in its vault",
        store.display()
    )]
    #[diagnostic(
        code(apprafter::secrets::override_refused),
        help("{}", override_refused_help(var))
    )]
    OverrideOnLockedStore { var: &'static str, store: PathBuf },

    /// Every attempt at the prompt failed (D.4a T61).
    #[error("{}", unlock_failed_message(store, *attempts))]
    #[diagnostic(
        code(apprafter::secrets::unlock_failed),
        help("{}", unlock_failed_help(*stopped_before))
    )]
    UnlockFailed {
        store: PathBuf,
        attempts: u32,
        stopped_before: Option<ProvisionedBy>,
    },

    /// A write prepared for one lock state of the store found another under the store's write
    /// lock — a `lock on` or `lock off` finished meanwhile — or a key for a vault that was
    /// replaced (D.4a T41, critic G3). Nothing was written.
    #[error("{}", store_changed_message(store, *prepared, *now))]
    #[diagnostic(
        code(apprafter::secrets::store_changed),
        help(
            "Another AppRafter process turned this store's lock on or off meanwhile (`apprafter \
             lock on` / `apprafter lock off`). Run this command again: it reads the store as it \
             is now."
        )
    )]
    StoreChanged {
        store: PathBuf,
        prepared: SecretsModeKind,
        now: SecretsModeKind,
    },

    /// A target's credentials file names this store's vault, which has no entry for it: an
    /// older AppRafter renamed the target's directory and not its vault entry (D.4a T43, G10).
    #[error(
        "the vault of the target store at {} holds no token for `{target}`, although its \
         credentials file says it does",
        store.display()
    )]
    #[diagnostic(
        code(apprafter::secrets::token_missing_from_vault),
        help(
            "If an older AppRafter renamed this target, rename it back to its old name with this \
             AppRafter, then rename it again. Otherwise store its token again: `apprafter target \
             add {target} --renew` (at a terminal it asks for the new token without showing it; \
             in a script, `--token`)."
        )
    )]
    TokenMissingFromVault { target: String, store: PathBuf },

    /// `lock off` read back what it wrote, and a file did not hold it (D.4a rule 4): the run
    /// stopped with the vault kept, and the next one finishes.
    #[error(
        "read back after the lock was turned off, {what} does not hold what was written; the \
         vault at {} was kept",
        vault.display()
    )]
    #[diagnostic(
        code(apprafter::secrets::files_not_kept),
        help(
            "Nothing was lost: the vault still holds every secret. Run `apprafter lock off` \
             again: it finishes from where this run stopped. If it stops here again, check the \
             disk the target store is on: it may be full, or another program may be writing \
             these files."
        )
    )]
    FilesNotKept { what: String, vault: PathBuf },

    /// A vault that was not written (D.4a T27, W7): too large, or no safe way to create it.
    #[error("{}", vault_unwritable_message(path, problem))]
    #[diagnostic(
        code(apprafter::secrets::vault_unwritable),
        help("{}", vault_unwritable_help(problem))
    )]
    VaultUnwritable {
        path: PathBuf,
        problem: VaultWriteProblem,
    },

    /// A vault that cannot be read: missing, not a vault, newer, damaged, or another store's.
    #[error("{}", vault_unreadable_message(path, problem, detail))]
    #[diagnostic(
        code(apprafter::secrets::vault_unreadable),
        help("{}", vault_unreadable_help(path, problem))
    )]
    VaultUnreadable {
        path: PathBuf,
        problem: VaultProblem,
        detail: String,
    },

    /// `store.yaml` cannot be read, or says what no AppRafter writes — a newer format, an
    /// unknown `secrets:` value, a lock without an id, a vault beside no lock (D.4a T31). The
    /// store is then treated as locked: none of its secrets is used.
    #[error(
        "{} cannot be read, so whether this target store's secrets are locked is unknown: \
         {detail}",
        path.display()
    )]
    #[diagnostic(
        code(apprafter::secrets::store_file_unreadable),
        help(
            "AppRafter treats the store as locked and uses none of its secrets until this file \
             reads again. Fix it by hand (`secrets:` is `files`, `locking`, `locked` or \
             `unlocking`) or restore it from a backup; never delete it while the store's \
             `vault` exists."
        )
    )]
    StoreFileUnreadable { path: PathBuf, detail: String },

    /// An earlier `lock on` or `lock off` did not finish: every secret read refuses until one
    /// finishes or backs out (D.4a T31, T37).
    #[error(
        "an earlier `{}` on the target store at {} did not finish",
        phase.command(),
        store.display()
    )]
    #[diagnostic(
        code(apprafter::secrets::lock_incomplete),
        help("{}", lock_incomplete_help(*phase))
    )]
    LockIncomplete { store: PathBuf, phase: LockPhase },

    /// A slot refused before any key was derived (D.4a T22, T63).
    #[error("{}", slot_refused_message(store, *reason))]
    #[diagnostic(
        code(apprafter::secrets::slot_refused),
        help("{}", slot_refused_help(*reason))
    )]
    SlotRefused { store: PathBuf, reason: SlotRefusal },

    /// `lock slot remove`'s slot matches no slot of the store, or more than one (D.4b B21,
    /// B29): `matches` are the ids of the slots it could mean, empty for none. Nothing was
    /// written.
    #[error("{}", slot_not_found_message(store, given, matches))]
    #[diagnostic(
        code(apprafter::secrets::slot_not_found),
        help(
            "`apprafter lock slot list` shows each slot's id: give 4 or more of its characters, \
             or a kind the store has exactly one slot of (`password`, `os-password`)."
        )
    )]
    SlotNotFound {
        store: PathBuf,
        given: String,
        matches: Vec<String>,
    },

    /// The account password was not taken for a slot (D.4b B29, B74, B75, B88); `messages`
    /// are the system's own words, when it gave any (a locked account says why). Nothing was
    /// written.
    #[error("{}", account_password_refused_message(reason, messages))]
    #[diagnostic(
        code(apprafter::secrets::account_password_refused),
        help("{}", account_password_refused_help(reason))
    )]
    AccountPasswordRefused {
        reason: AccountPasswordRefusal,
        messages: Vec<String>,
    },

    /// A plaintext cached secret in a locked store's state (D.4a T38): an older AppRafter's
    /// write, or a forgery; never used, never sealed later.
    #[error(
        "the {} cached for target `{target}` is not encrypted, but this target store is locked: \
         it is not used",
        secret.noun()
    )]
    #[diagnostic(
        code(apprafter::secrets::cache_not_sealed),
        help("{}", cache_not_sealed_help(secret, target))
    )]
    CacheNotSealed {
        secret: crate::secrets::CachedSecret,
        target: String,
    },

    /// A clear token in a locked store's `credentials.yaml` (an older AppRafter's `target add`,
    /// or planted): never used, never adopted (D.4a T42).
    #[error(
        "the credentials file of target `{target}` holds a token in clear, but this target store \
         is locked: that token is not used ({})",
        path.display()
    )]
    #[diagnostic(
        code(apprafter::secrets::token_outside_vault),
        help(
            "A locked store takes tokens only from its vault; this one was written by an older \
             AppRafter or by another program. `apprafter target add {target} --renew` stores a \
             token in the vault and replaces the file (at a terminal it asks for the new token \
             without showing it; in a script, `--token`). That token was on disk in clear: if it \
             is a real one, delete it in the Hetzner Console once the new one works."
        )
    )]
    TokenOutsideVault { target: String, path: PathBuf },

    /// A marker naming another store's vault: the target's directory came from it (D.4a T34).
    #[error("the token of target `{target}` is in the vault of another target store (id {id})")]
    #[diagnostic(
        code(apprafter::secrets::token_in_another_vault),
        help(
            "The target's files came from the target store with id {id}, whose vault holds its \
             token. Store the token here: `apprafter target add {target} --renew` (at a \
             terminal it asks for the new token without showing it; in a script, `--token`)."
        )
    )]
    TokenInAnotherVault { target: String, id: String },

    /// A vault or `store.yaml` write without the store's held write lock (D.4a T28): a lost
    /// race would overwrite the only copy of every token.
    #[error(
        "the vault of the target store at {} is written only under the store's write lock, and \
         this filesystem gives none",
        store.display()
    )]
    #[diagnostic(
        code(apprafter::secrets::vault_needs_write_lock),
        help(
            "The vault holds the only copy of these secrets, so it is never written without the \
             lock. Keep the target store on a local filesystem with file locks \
             (`APPRAFTER_CONFIG_DIR` moves it)."
        )
    )]
    VaultNeedsWriteLock { store: PathBuf },

    /// The operating system's random numbers failed (D.4a §3.7): no key, no nonce, no write,
    /// and no fallback generator.
    #[error(
        "the operating system's random number generator failed, so nothing was written: {detail}"
    )]
    #[diagnostic(
        code(apprafter::secrets::random_unavailable),
        help(
            "AppRafter makes keys only from the operating system's random numbers, with no \
             fallback. Try again; if it keeps failing, the system's entropy source is broken."
        )
    )]
    RandomUnavailable { detail: String },

    /// The runtime directory, where a decrypted kubeconfig is written for kubectl, belongs to
    /// another account (WI-473, Windows): that account can grant itself access to it whatever
    /// DACL it has, and so replace the kubeconfig before kubectl reads it. Nothing is written.
    #[error(
        "{} belongs to another account, so AppRafter writes no decrypted kubeconfig there",
        path.display()
    )]
    #[diagnostic(
        code(apprafter::secrets::runtime_dir_foreign),
        help(
            "Another account could replace a kubeconfig in it before kubectl reads it. Remove \
             the directory, and AppRafter makes it again, owned by you (an administrator can \
             remove it if Windows refuses you); or set `APPRAFTER_CONFIG_DIR` to a folder only \
             you can write."
        )
    )]
    RuntimeDirForeign { path: PathBuf },

    /// A folder of the target store another account owns, or that other accounts can write
    /// (WI-475): they could replace the store's files there — a token, a cached kubeconfig, the
    /// vault — so no secret of the store is used or written.
    #[error("{}", store_not_private_message(path, *problem))]
    #[diagnostic(
        code(apprafter::secrets::store_not_private),
        help("{}", store_not_private_help(path, *problem))
    )]
    StoreNotPrivate {
        path: PathBuf,
        problem: StoreFolderProblem,
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

    /// D.4a T15: the core's `token_missing`, for a command that still returns
    /// `cli_core::Result` — the core's code, message and CLI help, word for word.
    #[test]
    fn a_target_without_a_token_has_the_cores_code_and_help() {
        let err = CliError::TokenNotStored {
            name: "prod".into(),
        };
        assert_eq!(code_of(&err), "apprafter::target::token_missing");
        assert_eq!(
            err.to_string(),
            "target `prod` has no provider token stored"
        );
        assert_eq!(
            help_of(&err),
            "Run `apprafter target add prod --renew` to store one (at a terminal it asks for the \
             new token without showing it; in a script, `--token`), or set `HCLOUD_TOKEN` for \
             this invocation."
        );
    }

    /// d4a-d Task 13b: the three refusals for a token this store does not use name the renewal
    /// with no token on the command line (other processes can read it there, and the shell's
    /// history keeps it): at a terminal it asks without showing it. A token found in clear is
    /// named for what it may be, a real one to delete once the new one works.
    #[test]
    fn the_unused_token_refusals_name_a_renewal_that_asks() {
        let cases = [
            CliError::TokenOutsideVault {
                target: "prod".into(),
                path: "/s/targets/prod/credentials.yaml".into(),
            },
            CliError::TokenInAnotherVault {
                target: "prod".into(),
                id: "0123456789abcdef".into(),
            },
            CliError::TokenMissingFromVault {
                target: "prod".into(),
                store: "/s".into(),
            },
        ];
        for err in &cases {
            let help = help_of(err)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                help.contains("`apprafter target add prod --renew` "),
                "{help}"
            );
            assert!(
                help.contains(
                    "(at a terminal it asks for the new token without showing it; in a script, \
                     `--token`)"
                ),
                "{help}"
            );
            assert!(!help.contains("--renew --token"), "{help}");
        }
        assert!(
            help_of(&cases[0]).contains(
                "That token was on disk in clear: if it is a real one, delete it in the Hetzner \
                 Console once the new one works."
            ),
            "{}",
            help_of(&cases[0])
        );
    }

    /// D.4a T61: a prompt the person stopped is the core's `op::cancelled` on a legacy arm too.
    #[test]
    fn a_cancelled_prompt_has_the_cores_code() {
        let err = CliError::Cancelled;
        assert_eq!(code_of(&err), "apprafter::op::cancelled");
        assert_eq!(err.to_string(), "operation cancelled");
        assert_eq!(
            help_of(&err),
            "The command was interrupted; nothing after that point ran."
        );
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
    /// it, and offers the target's re-creation: since WI-458 `target remove` takes a target it
    /// cannot read, so the help names that, never deleting a directory by hand.
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
            assert!(help.contains("`apprafter target remove prod`"), "{help}");
            assert!(
                help.contains("`apprafter target add prod --provider hetzner-cloud …`"),
                "{help}"
            );
            // WI-458 review #0/#2: the removal deletes the target's local state too — the only
            // local record of its server — so the help says what goes, to fix or restore the
            // file first when a server is recorded, and how the record comes back.
            for says in [
                "local state",
                "record of its server",
                "cached kubeconfig",
                "Argo CD password",
                "If a server is recorded, fix or restore the file first",
                "`apprafter import --target prod`",
            ] {
                assert!(help.contains(says), "{says}: {help}");
            }
            for not_this in ["$XDG_CONFIG_HOME", "directory", "both of its files"] {
                assert!(!help.contains(not_this), "{not_this}: {help}");
            }
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

    /// WI-458 review #6: an I/O error on a target's own file shows as a plain I/O error does —
    /// code, cause and help — and says whose file it is, for the desktop. WI-482: its message
    /// names the file too, which the OS message does not.
    #[test]
    fn a_target_file_io_error_shows_as_a_plain_io_error_naming_its_file() {
        let os = || io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        let plain = CliError::Io(os());
        let target = CliError::TargetFileIo {
            path: PathBuf::from("/s/targets/prod/config.yaml"),
            target: "prod".into(),
            source: os(),
        };
        assert_eq!(code_of(&target), "apprafter::io::error");
        assert_eq!(code_of(&target), code_of(&plain));
        assert_eq!(plain.to_string(), "io error: denied");
        assert_eq!(
            target.to_string(),
            "io error: /s/targets/prod/config.yaml: denied"
        );
        assert_eq!(help_of(&target), help_of(&plain));
        let cause = |e: &CliError| std::error::Error::source(e).map(ToString::to_string);
        assert_eq!(cause(&target), cause(&plain));
        assert_eq!(cause(&target).as_deref(), Some("denied"));
    }

    /// d4a-h N3 (the re-walk's P-3x): a file or folder of the store this account cannot read is
    /// named once, its OS message the cause and not repeated in the message, under a help for a
    /// folder or a file — never `chmod 0600` on credentials. The code is any I/O error's.
    #[test]
    fn a_store_path_this_account_cannot_read_is_named_once_with_its_own_help() {
        let os = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        let e = CliError::StorePathIo {
            path: PathBuf::from("/s/targets"),
            source: os,
        };
        assert_eq!(code_of(&e), "apprafter::io::error");
        assert_eq!(e.to_string(), "cannot read /s/targets");
        let cause = std::error::Error::source(&e).map(ToString::to_string);
        assert_eq!(cause.as_deref(), Some("denied"));
        let help = help_of(&e);
        assert!(help.contains("folder"), "{help}");
        assert!(!help.contains("chmod 0600"), "{help}");
    }

    /// Bug 8: `--force` now keeps the stored values, so it refuses an unreadable target (it
    /// always did since it read both files) and cannot be the way to rewrite one. WI-458:
    /// `target remove` can, so the help names it rather than deleting a directory by hand, and
    /// says what `invalid_config`'s help says of that removal: it deletes the target's local
    /// state too, the only local record of its server (review #0/#2).
    #[test]
    fn yaml_help_never_sends_the_reader_to_force() {
        let err = CliError::from(serde_yaml::from_str::<u8>("[").unwrap_err());
        assert_eq!(code_of(&err), "apprafter::io::yaml");
        let help = help_of(&err);
        assert!(!help.contains("<name> --force"), "{help}");
        assert!(
            help.contains("by hand") && help.contains("restore it from a backup"),
            "{help}"
        );
        assert!(help.contains("`apprafter target remove <name>`"), "{help}");
        assert!(!help.contains("directory"), "{help}");
        for says in [
            "local state",
            "record of its server",
            "cached kubeconfig",
            "Argo CD password",
            "If a server is recorded, fix or restore the file first",
            "`apprafter import --target <name>`",
        ] {
            assert!(help.contains(says), "{says}: {help}");
        }
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
        for why in [
            "wrong",
            "revoked",
            "rotated",
            "`apprafter target add <name> --renew` replaces it",
            "in a script, `--token`",
        ] {
            assert!(line("401").contains(why), "{why}: {help}");
        }
        // d4a-d Task 13b: never a token on the command line, at a terminal.
        assert!(!line("401").contains("--renew --token"), "{help}");
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
            help.contains(
                "`apprafter target add <name> --renew` instead of re-creating the target (at a \
                 terminal it asks for the new token without showing it; in a script, `--token`)"
            ),
            "missing rotation hint: {help}"
        );
        assert!(!help.contains("--renew --token"), "{help}");
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
        // Where a new token comes from, in the one shared wording.
        assert!(
            help.contains(crate::target::HETZNER_API_TOKENS_PAGE),
            "the token page: {help}"
        );
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
        // Transport-error case (no HTTP status): the Hetzner client's
        // classification of a request that got no answer.
        let inner = TransportFailure {
            endpoint: "https://api.hetzner.cloud/v1/servers".into(),
            detail: "Connection Failed: Connect error: Connection refused".into(),
        };
        let err = CliError::ProviderApiUnreachable {
            provider: "hetzner-cloud".into(),
            cause: Box::new(inner),
        };
        assert_eq!(code_of(&err), "apprafter::target::provider_unreachable");
        // WI-453: every request that gets no answer lands here — `apply`, a catalogue read —
        // so neither the message nor the help may say it was the token check.
        assert_eq!(
            err.to_string(),
            "provider `hetzner-cloud` API was unreachable"
        );
        let help = help_of(&err);
        assert!(!help.contains("credential check could not"), "{help}");
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

    /// WI-478: an age key file with no key, or with more than one, is its own code: the message
    /// names the file and what is wrong with it, and the help the format it is read in.
    #[test]
    fn an_age_key_file_with_no_key_or_more_than_one_says_so() {
        let path = PathBuf::from("/k/age.key");
        let none = CliError::AgeKeyIdentityCount {
            path: path.clone(),
            found: 0,
        };
        let two = CliError::AgeKeyIdentityCount { path, found: 2 };
        for err in [&none, &two] {
            assert_eq!(code_of(err), "apprafter::secrets::age_key_identity_count");
            let help = help_of(err);
            for part in [
                "`AGE-SECRET-KEY-1…`",
                "start with `#` and blank lines are skipped",
                "`age-keygen -o <file>`",
                "`APPRAFTER_AGE_KEY`",
                "left as it is",
            ] {
                assert!(help.contains(part), "{part}: {help}");
            }
        }
        assert_eq!(
            none.to_string(),
            "the age key file /k/age.key holds no key: every line in it is blank or a comment"
        );
        assert_eq!(
            two.to_string(),
            "the age key file /k/age.key has 2 lines that are neither blank nor a comment, and \
             AppRafter takes a file with exactly one: the key"
        );
    }

    /// WI-457: the CLI's own commands raise the core's code and message, and the help names
    /// the way back for the target whose cache it is: restore the key, or fetch the kubeconfig
    /// again under a new one (`kubeconfig --refresh`, which asks first).
    #[test]
    fn a_missing_age_key_names_the_recovery_for_its_target() {
        let err = CliError::AgeKeyMissing {
            path: "/k/age.key".into(),
            target: "prod".into(),
            stopped: None,
            lost_vault: None,
        };
        assert_eq!(code_of(&err), "apprafter::secrets::age_key_missing");
        assert_eq!(
            err.to_string(),
            "no age key at /k/age.key; the cached secrets cannot be decrypted"
        );
        let help = help_of(&err);
        for part in [
            "APPRAFTER_AGE_KEY",
            "`apprafter kubeconfig --refresh --target prod`",
            "new key",
        ] {
            assert!(help.contains(part), "{part:?} missing: {help}");
        }
        assert!(!help.contains("file an issue"), "{help}");
        assert_eq!(help, age_key_missing_help(Some("prod")));
        assert!(
            age_key_missing_help(None).contains("`apprafter kubeconfig --refresh --target <name>`"),
            "{}",
            age_key_missing_help(None)
        );
    }

    /// WI-457: before `up` or `restore --reprovision` provisions anything, a lost key stops it
    /// with nothing created. The help says so, and how to go on: the key back, or `kubeconfig
    /// --refresh` for a target with something cached under the lost key (`up`'s own target has
    /// no server yet), then the command that stopped, again. It claims no server. The message
    /// and the code stay the core's.
    #[test]
    fn a_missing_age_key_before_a_provision_says_nothing_was_created_and_how_to_go_on() {
        let before = |by| CliError::AgeKeyMissing {
            path: "/k/age.key".into(),
            target: "prod".into(),
            stopped: Some(ProvisionStop::Before {
                by,
                recover: "staging".into(),
            }),
            lost_vault: None,
        };
        for (by, stopped, again) in [
            (
                ProvisionedBy::Up,
                "`apprafter up` stopped before provisioning, so nothing was created",
                "Then run `apprafter up --target prod` again.",
            ),
            (
                ProvisionedBy::RestoreReprovision,
                "`apprafter restore --reprovision` stopped before provisioning, so nothing was \
                 created",
                "Then run the same `apprafter restore` command again (the same repository and \
                 options, with `--reprovision --target prod`).",
            ),
        ] {
            let err = before(by);
            assert_eq!(code_of(&err), "apprafter::secrets::age_key_missing");
            assert_eq!(
                err.to_string(),
                "no age key at /k/age.key; the cached secrets cannot be decrypted"
            );
            let help = help_of(&err);
            for part in [
                stopped,
                "`APPRAFTER_AGE_KEY`",
                "`apprafter kubeconfig --refresh --target staging`",
                "lists what the new key cannot read and asks first",
                again,
            ] {
                assert!(help.contains(part), "{by:?}: {part:?} missing: {help}");
            }
            for wrong in [
                "bills",
                "exists",
                "--refresh --target prod",
                "finds the server",
            ] {
                assert!(!help.contains(wrong), "{by:?}: {wrong:?} in: {help}");
            }
        }
    }

    /// d4a-h F13's residue (the re-walk's C-12 step 3): after a locked store's vault is lost, its
    /// caches were encrypted to that vault, never to an age key. The help names the vault and
    /// the way back — never "restore the age key file the cache was encrypted with" or
    /// `APPRAFTER_AGE_KEY` — alone and when `up` or `restore --reprovision` stopped on it.
    #[test]
    fn a_missing_age_key_whose_caches_record_a_lost_vault_names_the_vault() {
        let err = |stopped| CliError::AgeKeyMissing {
            path: "/k/age.key".into(),
            target: "prod".into(),
            stopped,
            lost_vault: Some("0123456789abcdef".into()),
        };
        let alone = help_of(&err(None));
        assert_eq!(
            alone,
            age_key_missing_lost_vault_help("prod", "0123456789abcdef")
        );
        for part in [
            "encrypted to the key of a target store vault that is no longer here (store id \
             0123456789abcdef)",
            "put them back and unlock",
            "`apprafter kubeconfig --refresh --target prod`",
        ] {
            assert!(alone.contains(part), "{part:?} missing: {alone}");
        }
        let stops = [
            Some(ProvisionStop::Before {
                by: ProvisionedBy::Up,
                recover: "prod".into(),
            }),
            Some(ProvisionStop::After {
                by: ProvisionedBy::Up,
            }),
        ];
        for stopped in &stops {
            let help = help_of(&err(stopped.clone()));
            assert!(
                help.contains(
                    "put back the target store vault the cache was encrypted to (store id \
                     0123456789abcdef)"
                ),
                "{stopped:?}: {help}"
            );
            assert!(
                help.contains("If that vault is lost"),
                "{stopped:?}: {help}"
            );
        }
        for help in [alone]
            .into_iter()
            .chain(stops.map(|stopped| help_of(&err(stopped))))
        {
            for wrong in [
                "Restore the age key",
                "restore the age key",
                "APPRAFTER_AGE_KEY",
            ] {
                assert!(!help.contains(wrong), "{wrong:?} in: {help}");
            }
        }
    }

    /// WI-457 review #6: after `up` or `restore --reprovision` provisioned the server, the help
    /// says the server exists and bills, that cluster-bootstrap did not run, and how to go on:
    /// the key back or `kubeconfig --refresh`, then the command that stopped, again. The message
    /// and the code stay the core's.
    #[test]
    fn a_missing_age_key_after_a_provision_says_what_it_left_and_how_to_go_on() {
        let after = |by| CliError::AgeKeyMissing {
            path: "/k/age.key".into(),
            target: "prod".into(),
            stopped: Some(ProvisionStop::After { by }),
            lost_vault: None,
        };
        for (by, stopped, again) in [
            (
                ProvisionedBy::Up,
                "`apprafter up` stopped after provisioning",
                "Then run `apprafter up --target prod` again",
            ),
            (
                ProvisionedBy::RestoreReprovision,
                "`apprafter restore --reprovision` stopped after provisioning",
                "Then run the same `apprafter restore` command again (the same repository and \
                 options, with `--reprovision --target prod`)",
            ),
        ] {
            let err = after(by);
            assert_eq!(code_of(&err), "apprafter::secrets::age_key_missing");
            assert_eq!(
                err.to_string(),
                "no age key at /k/age.key; the cached secrets cannot be decrypted"
            );
            let help = help_of(&err);
            for part in [
                stopped,
                "the server of target `prod` exists, and Hetzner bills for it",
                "cluster-bootstrap did not run",
                "`APPRAFTER_AGE_KEY`",
                "`apprafter kubeconfig --refresh --target prod`",
                again,
                "it finds the server it provisioned instead of creating another",
            ] {
                assert!(help.contains(part), "{by:?}: {part:?} missing: {help}");
            }
        }
    }

    /// WI-457: a cached secret the key there cannot open names the command that fetches that
    /// secret again under it, for the target whose cache it is (review #7: `argocd-password`
    /// takes `--target`, so the help never depends on which target is active).
    #[test]
    fn an_undecryptable_cache_names_how_to_fetch_it_again() {
        use crate::secrets::CachedSecret;
        let err = |secret| CliError::CacheUndecryptable {
            secret,
            target: "prod".into(),
            path: "/k/age.key".into(),
            detail: "age decrypt: No matching keys found".into(),
            key: crate::secrets::CacheKeyKind::SharedFile,
            reason: crate::secrets::UndecryptableWhy::NotForThisKey,
        };
        let kube = err(CachedSecret::Kubeconfig);
        assert_eq!(code_of(&kube), "apprafter::secrets::cache_undecryptable");
        assert_eq!(
            kube.to_string(),
            "the kubeconfig cached for target `prod` cannot be decrypted with the age key at \
             /k/age.key: age decrypt: No matching keys found"
        );
        let help = help_of(&kube);
        assert!(
            help.contains("APPRAFTER_AGE_KEY")
                && help.contains("`apprafter kubeconfig --refresh --target prod`"),
            "{help}"
        );
        let argocd = err(CachedSecret::ArgocdPassword);
        assert!(
            argocd
                .to_string()
                .starts_with("the Argo CD admin password cached for target `prod`"),
            "{argocd}"
        );
        let help = help_of(&argocd);
        assert!(
            help.contains("`apprafter argocd-password --refresh --target prod`"),
            "{help}"
        );
    }

    /// D.4a T38, §3.12: on a locked store the only key is the store's own, and the way back is
    /// the fetch alone — never `APPRAFTER_AGE_KEY`, which a locked store refuses (ADR 0068 §6).
    #[test]
    fn a_cache_the_store_key_cannot_open_names_only_the_refresh() {
        use crate::secrets::{CacheKeyKind, CachedSecret, UndecryptableWhy};
        let err = CliError::CacheUndecryptable {
            secret: CachedSecret::Kubeconfig,
            target: "prod".into(),
            path: "/s/vault".into(),
            detail: "age decrypt: No matching keys found".into(),
            key: CacheKeyKind::StoreVault,
            reason: UndecryptableWhy::NotForThisKey,
        };
        assert_eq!(code_of(&err), "apprafter::secrets::cache_undecryptable");
        assert_eq!(
            err.to_string(),
            "the kubeconfig cached for target `prod` cannot be decrypted with this target \
             store's own key, in its vault /s/vault: age decrypt: No matching keys found"
        );
        let help = help_of(&err);
        assert!(
            help.contains("`apprafter kubeconfig --refresh --target prod`")
                && help.contains("the store's key"),
            "{help}"
        );
        assert!(!help.contains("APPRAFTER_AGE_KEY"), "{help}");
    }

    /// D.4b B86: a cache whose record names a store key a key change of this store retired
    /// says so — what the record says, that the copy may be damaged (critic round, finding 3),
    /// and the refresh — and its reason reads `retired_key` on the wire.
    #[test]
    fn a_cache_sealed_to_a_retired_store_key_names_the_key_change_and_the_refresh() {
        use crate::secrets::{CacheKeyKind, CachedSecret, UndecryptableWhy};
        let err = CliError::CacheUndecryptable {
            secret: CachedSecret::Kubeconfig,
            target: "prod".into(),
            path: "/s/vault".into(),
            detail: "age decrypt: No matching keys found".into(),
            key: CacheKeyKind::StoreVault,
            reason: UndecryptableWhy::RetiredKey,
        };
        assert_eq!(code_of(&err), "apprafter::secrets::cache_undecryptable");
        assert_eq!(
            err.to_string(),
            "the kubeconfig cached for target `prod` is recorded as sealed to the store key of \
             an earlier key change, which that change retired: this target store's own key, \
             in its vault /s/vault, does not decrypt it"
        );
        let help = help_of(&err);
        assert!(
            help.starts_with(
                "A key change (`apprafter lock rekey` or `apprafter lock recovery`) replaced \
                 this target store's own key"
            ),
            "{help}"
        );
        assert!(help.contains("— or the copy is damaged."), "{help}");
        assert!(
            help.ends_with(
                "`apprafter kubeconfig --refresh --target prod` fetches it from the node again \
                 and caches it under the store's key."
            ),
            "{help}"
        );
        assert!(!help.contains("APPRAFTER_AGE_KEY"), "{help}");
        assert_eq!(UndecryptableWhy::RetiredKey.as_str(), "retired_key");
    }

    /// D.4a §3.12: every lock refusal has its code, a message that names the store or the target
    /// (never a secret), and a help that says what to do — no catch-all "file an issue".
    #[test]
    fn the_lock_refusals_have_their_codes_messages_and_helps() {
        use crate::secrets::CachedSecret;
        let s = || PathBuf::from("/s");
        let cases: Vec<(CliError, &str, &str, &str)> = vec![
            (CliError::StoreLocked { store: s(), reason: LockedWhy::NoTerminal, stopped_before: None },
             "apprafter::secrets::store_locked",
             "the target store at /s is locked, and there is no unlock window here",
             "run `apprafter unlock` there"),
            (CliError::StoreLocked { store: s(), reason: LockedWhy::TimedOut, stopped_before: None },
             "apprafter::secrets::store_locked",
             "the target store at /s is locked, and nobody answered the unlock prompt within 5 minutes",
             "cannot ask for the lock password"),
            (CliError::StoreLocked { store: s(), reason: LockedWhy::NoTerminal, stopped_before: Some(ProvisionedBy::Up) },
             "apprafter::secrets::store_locked",
             "the target store at /s is locked, and there is no unlock window here",
             "`apprafter up` stopped before provisioning, so nothing was created"),
            (CliError::OverrideOnLockedStore { var: crate::HCLOUD_TOKEN_ENV, store: s() },
             "apprafter::secrets::override_refused",
             "HCLOUD_TOKEN is set, but the target store at /s is locked: a locked store uses only the secrets in its vault",
             "`APPRAFTER_CONFIG_DIR=<dir>`"),
            (CliError::OverrideOnLockedStore { var: crate::secrets::AGE_KEY_ENV, store: s() },
             "apprafter::secrets::override_refused",
             "APPRAFTER_AGE_KEY is set, but the target store at /s is locked: a locked store uses only the secrets in its vault",
             "the store's own age key, in its vault"),
            (CliError::UnlockFailed { store: s(), attempts: 3, stopped_before: None },
             "apprafter::secrets::unlock_failed",
             "3 attempts did not unlock the target store at /s",
             "The recovery key opens the store at the same prompt"),
            (CliError::UnlockFailed { store: s(), attempts: 1, stopped_before: Some(ProvisionedBy::RestoreReprovision) },
             "apprafter::secrets::unlock_failed",
             "1 attempt did not unlock the target store at /s",
             "`apprafter restore --reprovision` stopped before provisioning"),
            (CliError::StoreChanged { store: s(), prepared: SecretsModeKind::Files, now: SecretsModeKind::Locked },
             "apprafter::secrets::store_changed",
             "the lock state of the target store at /s changed while this command ran (files → locked); nothing was written",
             "Run this command again"),
            (CliError::StoreChanged { store: s(), prepared: SecretsModeKind::Locked, now: SecretsModeKind::Locked },
             "apprafter::secrets::store_changed",
             "the vault of the target store at /s was replaced while this command ran; nothing was written",
             "Run this command again"),
            (CliError::FilesNotKept { what: "the credentials file of `prod`".into(), vault: "/s/vault".into() },
             "apprafter::secrets::files_not_kept",
             "read back after the lock was turned off, the credentials file of `prod` does not hold what was written; the vault at /s/vault was kept",
             "Run `apprafter lock off` again"),
            (CliError::TokenMissingFromVault { target: "prod".into(), store: s() },
             "apprafter::secrets::token_missing_from_vault",
             "the vault of the target store at /s holds no token for `prod`, although its credentials file says it does",
             "rename it back to its old name"),
            (CliError::VaultUnwritable { path: "/s/vault".into(), problem: VaultWriteProblem::TooLarge { limit: 2_097_152 } },
             "apprafter::secrets::vault_unwritable",
             "the vault /s/vault would hold more than 2097152 bytes, the most a vault may: nothing was written",
             "`apprafter target remove <name>`"),
            (CliError::VaultUnwritable { path: s(), problem: VaultWriteProblem::NoReplaceUnsupported { fs: Some("exfat".into()) } },
             "apprafter::secrets::vault_unwritable",
             "a vault cannot be created in /s (exfat): this filesystem cannot create a file without risking replacing one, so nothing was written",
             "hard links"),
            (CliError::VaultUnwritable { path: "/s/vault".into(), problem: VaultWriteProblem::Refused { what: RefusedInput::Token } },
             "apprafter::secrets::vault_unwritable",
             "the vault /s/vault cannot hold this write (a Hetzner token is 1 to 16384 bytes): nothing was written",
             "check what was pasted"),
            (CliError::VaultUnwritable { path: "/s/vault".into(), problem: VaultWriteProblem::Refused { what: RefusedInput::NameTaken } },
             "apprafter::secrets::vault_unwritable",
             "the vault /s/vault cannot hold this write (the new target name already has entries in the vault): nothing was written",
             "choose another name"),
            (CliError::VaultUnreadable { path: "/s/vault".into(), problem: VaultProblem::Missing, detail: String::new() },
             "apprafter::secrets::vault_unreadable",
             "the vault /s/vault is missing",
             "restore /s/vault from a backup"),
            (CliError::VaultUnreadable { path: "/s/vault".into(), problem: VaultProblem::NotAVault, detail: "x".into() },
             "apprafter::secrets::vault_unreadable",
             "/s/vault is not an AppRafter vault: x",
             "nothing else holds these secrets"),
            (CliError::VaultUnreadable { path: "/s/vault".into(), problem: VaultProblem::NewerFormat { version: 2 }, detail: "x".into() },
             "apprafter::secrets::vault_unreadable",
             "the vault /s/vault is in format 2, which this AppRafter does not read: x",
             "Upgrade AppRafter"),
            (CliError::VaultUnreadable { path: "/s/vault".into(), problem: VaultProblem::Damaged { what: DamageKind::Tampered }, detail: "x".into() },
             "apprafter::secrets::vault_unreadable",
             "the vault /s/vault is damaged: x",
             "never delete it"),
            (CliError::VaultUnreadable { path: "/s/vault".into(), problem: VaultProblem::OtherStore, detail: "x".into() },
             "apprafter::secrets::vault_unreadable",
             "the vault /s/vault belongs to another target store: x",
             "from the same backup"),
            (CliError::StoreFileUnreadable { path: "/s/store.yaml".into(), detail: "x".into() },
             "apprafter::secrets::store_file_unreadable",
             "/s/store.yaml cannot be read, so whether this target store's secrets are locked is unknown: x",
             "never delete it while the store's `vault` exists"),
            (CliError::LockIncomplete { store: s(), phase: LockPhase::Locking },
             "apprafter::secrets::lock_incomplete",
             "an earlier `lock on` on the target store at /s did not finish",
             "`apprafter lock off` backs out of it"),
            (CliError::LockIncomplete { store: s(), phase: LockPhase::Unlocking },
             "apprafter::secrets::lock_incomplete",
             "an earlier `lock off` on the target store at /s did not finish",
             "Run `apprafter lock off` again"),
            (CliError::LockIncomplete { store: s(), phase: LockPhase::Rotating },
             "apprafter::secrets::lock_incomplete",
             "an earlier `lock rekey` or `lock recovery` on the target store at /s did not finish",
             "`apprafter lock off` returns the secrets to files"),
            (CliError::SlotRefused { store: s(), reason: SlotRefusal::KdfTooWeak },
             "apprafter::secrets::slot_refused",
             "the lock password of the target store at /s is kept with key-derivation settings weaker than this AppRafter accepts",
             "`apprafter lock rekey`"),
            (CliError::SlotRefused { store: s(), reason: SlotRefusal::KdfOutOfRange },
             "apprafter::secrets::slot_refused",
             "the lock password of the target store at /s is kept with key-derivation settings beyond what this AppRafter computes",
             "`apprafter lock rekey`"),
            (CliError::SlotRefused { store: s(), reason: SlotRefusal::NoUsableSlot },
             "apprafter::secrets::slot_refused",
             "the target store at /s has no slot this AppRafter can open with that secret",
             "only after an upgrade"),
            (CliError::SlotRefused { store: s(), reason: SlotRefusal::LastRecoverySlot },
             "apprafter::secrets::slot_refused",
             "the recovery key slot of the target store at /s is its last one",
             "`apprafter lock recovery` replaces it"),
            (CliError::SlotRefused { store: s(), reason: SlotRefusal::NoUsableSlotLeft },
             "apprafter::secrets::slot_refused",
             "removing that slot would leave the target store at /s with no slot this AppRafter can open",
             "add one before removing this one"),
            (CliError::SlotRefused { store: s(), reason: SlotRefusal::AlreadyHasPassword },
             "apprafter::secrets::slot_refused",
             "the target store at /s has a lock password already",
             "`apprafter lock rekey` changes it"),
            (CliError::SlotRefused { store: s(), reason: SlotRefusal::NotOnThisOs },
             "apprafter::secrets::slot_refused",
             "an account-password slot opens a target store only on Linux and macOS, and this computer runs Windows (the target store at /s)",
             "`apprafter lock slot add password`"),
            (CliError::SlotRefused { store: s(), reason: SlotRefusal::SlotsFull },
             "apprafter::secrets::slot_refused",
             "the vault of the target store at /s holds 16 ways to open it, the most a vault may",
             "`apprafter lock slot remove <id>`"),
            (CliError::SlotNotFound { store: s(), given: "ffff".into(), matches: Vec::new() },
             "apprafter::secrets::slot_not_found",
             "the target store at /s has no slot `ffff`",
             "`apprafter lock slot list` shows each slot's id"),
            (CliError::SlotNotFound { store: s(), given: "os-password".into(), matches: vec!["3a4b5c6d00000000000000000000000a".into(), "9c0d1e2f00000000000000000000000b".into()] },
             "apprafter::secrets::slot_not_found",
             "`os-password` could mean more than one slot of the target store at /s: 3a4b5c6d, 9c0d1e2f",
             "a kind the store has exactly one slot of"),
            (CliError::AccountPasswordRefused { reason: AccountPasswordRefusal::Wrong, messages: vec!["The account is locked due to 3 failed logins.".into()] },
             "apprafter::secrets::account_password_refused",
             "the system did not accept that account password; the system said: The account is locked due to 3 failed logins.",
             "If this account has no usable password"),
            (CliError::AccountPasswordRefused { reason: AccountPasswordRefusal::NotChecked, messages: Vec::new() },
             "apprafter::secrets::account_password_refused",
             "the system did not check the password that was typed: a fingerprint reader, another sign-in module or a second question took its place",
             "not a fingerprint"),
            (CliError::AccountPasswordRefused { reason: AccountPasswordRefusal::NoPassword, messages: Vec::new() },
             "apprafter::secrets::account_password_refused",
             "this account has no password the system can check",
             "`apprafter lock slot add password`"),
            (CliError::AccountPasswordRefused { reason: AccountPasswordRefusal::Unavailable { what: AccountCheckUnavailable::NoLibpam }, messages: Vec::new() },
             "apprafter::secrets::account_password_refused",
             "this computer cannot check your account password: libpam is not installed",
             "`apprafter lock on --lock-password`"),
            (CliError::AccountPasswordRefused { reason: AccountPasswordRefusal::Unavailable { what: AccountCheckUnavailable::NoNewPrivs }, messages: Vec::new() },
             "apprafter::secrets::account_password_refused",
             "AppRafter cannot check your account password safely while it runs with no_new_privs (a sandbox such as Flatpak or bubblewrap, or a service started with NoNewPrivileges=): the system may refuse even the right password and count it as a failed login",
             "`apprafter lock on --lock-password`"),
            (CliError::AccountPasswordRefused { reason: AccountPasswordRefusal::Unavailable { what: AccountCheckUnavailable::SystemError { name: "PAM_SYSTEM_ERR".into() } }, messages: Vec::new() },
             "apprafter::secrets::account_password_refused",
             "the system's password check failed (PAM_SYSTEM_ERR) without a verdict",
             "a separate lock password"),
            (CliError::AccountPasswordRefused { reason: AccountPasswordRefusal::Backoff { wait_secs: Some(540) }, messages: Vec::new() },
             "apprafter::secrets::account_password_refused",
             "an account-password check failed recently: the next is allowed in 9 min",
             "so that typos cannot lock your account"),
            (CliError::AccountPasswordRefused { reason: AccountPasswordRefusal::Backoff { wait_secs: None }, messages: Vec::new() },
             "apprafter::secrets::account_password_refused",
             "another AppRafter process is checking an account password right now",
             "run the command again then"),
            (CliError::AccountPasswordRefused { reason: AccountPasswordRefusal::Unusable { why: AccountPasswordUnusable::RecoveryKeyShape }, messages: Vec::new() },
             "apprafter::secrets::account_password_refused",
             "that account password has the shape of a recovery key, and a prompt reads such text as a recovery key",
             "a separate lock password"),
            (CliError::CacheNotSealed { secret: CachedSecret::Kubeconfig, target: "prod".into() },
             "apprafter::secrets::cache_not_sealed",
             "the kubeconfig cached for target `prod` is not encrypted, but this target store is locked: it is not used",
             "`apprafter kubeconfig --refresh --target prod`"),
            (CliError::TokenOutsideVault { target: "prod".into(), path: "/s/targets/prod/credentials.yaml".into() },
             "apprafter::secrets::token_outside_vault",
             "the credentials file of target `prod` holds a token in clear, but this target store is locked: that token is not used (/s/targets/prod/credentials.yaml)",
             "`apprafter target add prod --renew` stores a token in the vault and replaces the file"),
            (CliError::TokenInAnotherVault { target: "prod".into(), id: "0123456789abcdef".into() },
             "apprafter::secrets::token_in_another_vault",
             "the token of target `prod` is in the vault of another target store (id 0123456789abcdef)",
             "the target store with id 0123456789abcdef"),
            (CliError::VaultNeedsWriteLock { store: s() },
             "apprafter::secrets::vault_needs_write_lock",
             "the vault of the target store at /s is written only under the store's write lock, and this filesystem gives none",
             "never written without the lock"),
            (CliError::RandomUnavailable { detail: "getrandom: ENOSYS".into() },
             "apprafter::secrets::random_unavailable",
             "the operating system's random number generator failed, so nothing was written: getrandom: ENOSYS",
             "with no fallback"),
            (CliError::RuntimeDirForeign { path: "/s/run".into() },
             "apprafter::secrets::runtime_dir_foreign",
             "/s/run belongs to another account, so AppRafter writes no decrypted kubeconfig there",
             "Remove the directory, and AppRafter makes it again, owned by you"),
            (CliError::StoreNotPrivate { path: s(), problem: StoreFolderProblem::ForeignOwner },
             "apprafter::secrets::store_not_private",
             "/s belongs to another account, which could replace the target store's files there: AppRafter neither uses nor writes this store's secrets until that is fixed",
             "Move the target store to a folder only you can write (set `APPRAFTER_CONFIG_DIR` to it)"),
            (CliError::StoreNotPrivate { path: "/s/targets".into(), problem: StoreFolderProblem::WritableByOthers },
             "apprafter::secrets::store_not_private",
             "other accounts can write to /s/targets, so they could replace the target store's files there: AppRafter neither uses nor writes this store's secrets until that is fixed",
             "`apprafter doctor` lists every folder and file concerned"),
        ];
        for (err, code, message, help) in cases {
            assert_eq!(code_of(&err), code);
            assert_eq!(err.to_string(), message);
            let h = help_of(&err);
            assert!(h.contains(help), "{code}: {h}");
            assert!(!h.contains("file an issue"), "{code}: {h}");
        }
    }

    /// WI-545: each reason this computer cannot check the account password has its name on the
    /// wire (`fields.unavailable`), the desktop's to read; `no_new_privs` is the kernel's own.
    #[test]
    fn each_unavailable_check_has_its_wire_name() {
        for (what, name) in [
            (AccountCheckUnavailable::NoLibpam, "no_libpam"),
            (AccountCheckUnavailable::NoService, "no_service"),
            (AccountCheckUnavailable::NoCheckRoute, "no_check_route"),
            (AccountCheckUnavailable::NoAccount, "no_account"),
            (AccountCheckUnavailable::NoNewPrivs, "no_new_privs"),
            (AccountCheckUnavailable::NoMachineId, "no_machine_id"),
            (
                AccountCheckUnavailable::NoBackoffRecord,
                "no_backoff_record",
            ),
            (
                AccountCheckUnavailable::SystemError {
                    name: "PAM_SYSTEM_ERR".into(),
                },
                "system_error",
            ),
        ] {
            assert_eq!(what.as_str(), name, "{what:?}");
        }
    }

    /// D.4b-c3 (B75 (3), d4b-d's `pam::decide`): a stack that did not check the typed text is
    /// counted toward the back-off when PAM reported a failure — a second prompt refused —
    /// and not when it let the check pass. The words claim neither, nor that it passed.
    #[test]
    fn not_checked_claims_no_count_and_no_pass() {
        let err = CliError::AccountPasswordRefused {
            reason: AccountPasswordRefusal::NotChecked,
            messages: Vec::new(),
        };
        let (message, help) = (err.to_string(), help_of(&err));
        for words in [&message, &help] {
            assert!(!words.contains("counted"), "{words}");
            assert!(!words.contains("let the check pass"), "{words}");
        }
        assert!(help.contains("not a fingerprint"), "{help}");
        assert!(
            help.contains("`apprafter lock on --lock-password`"),
            "{help}"
        );
    }

    /// D.4b-e1a (d4b-c3's Deviation 10): an interrupted `lock on` is finished with the store's
    /// everyday secret — its lock password or, by default, the account password — and the
    /// recovery key; where `lock on` cannot finish it, `lock off` backs it out with the key.
    #[test]
    fn an_interrupted_lock_on_names_both_everyday_secrets_and_lock_off_where_it_cannot_finish() {
        let help = help_of(&CliError::LockIncomplete {
            store: PathBuf::from("/s"),
            phase: LockPhase::Locking,
        });
        assert!(
            help.contains("its lock password or your account password"),
            "{help}"
        );
        assert!(
            help.contains(
                "where `apprafter lock on` says this AppRafter cannot finish the run, \
                 `apprafter lock off` backs it out with the recovery key"
            ),
            "{help}"
        );
    }

    /// WI-475: the fix for a store folder another account owns or can write is in this
    /// platform's terms.
    #[test]
    fn a_store_folder_others_can_change_is_fixed_in_this_platforms_terms() {
        let help = |problem| {
            help_of(&CliError::StoreNotPrivate {
                path: "/s".into(),
                problem,
            })
        };
        let (owner, write) = (
            help(StoreFolderProblem::ForeignOwner),
            help(StoreFolderProblem::WritableByOthers),
        );
        if cfg!(windows) {
            assert!(
                owner.contains("Properties › Security › Advanced"),
                "{owner}"
            );
            assert!(
                write.contains("write access in the folder's Properties › Security"),
                "{write}"
            );
        } else {
            assert!(owner.contains("`sudo chown <your user> /s`"), "{owner}");
            assert!(write.contains("`chmod go-w /s`"), "{write}");
        }
        assert_eq!(StoreFolderProblem::ForeignOwner.as_str(), "foreign_owner");
        assert_eq!(
            StoreFolderProblem::WritableByOthers.as_str(),
            "writable_by_others"
        );
    }

    /// WI-457 review #5/#12: a declined new key is an error, so a caller capturing the
    /// kubeconfig on stdout never takes an empty one for success. It says what did not happen and
    /// both ways on: the old key back, or the same command agreed to.
    /// WI-457 follow-up: phase 2's wait for the node ran out after `up` or `restore
    /// --reprovision` provisioned. The help says the server exists and bills, that
    /// cluster-bootstrap did not run, and to run the same command again, which finds the server;
    /// the message keeps the wait, the count and the last error.
    #[test]
    fn a_wait_for_the_node_that_ran_out_says_what_it_left_and_to_run_the_command_again() {
        for (by, stopped, again, goes_on) in [
            (
                ProvisionedBy::Up,
                "`apprafter up` stopped after provisioning",
                "To go on, run `apprafter up --target prod` again",
                "goes on with cluster-bootstrap.",
            ),
            (
                ProvisionedBy::RestoreReprovision,
                "`apprafter restore --reprovision` stopped after provisioning",
                "To go on, run the same `apprafter restore` command again (the same repository \
                 and options, with `--reprovision --target prod`)",
                "goes on with cluster-bootstrap and the restore.",
            ),
        ] {
            let err = CliError::K3sNotReady {
                target: "prod".into(),
                by,
                waited_secs: 300,
                attempts: 30,
                last_error: "apiserver 203.0.113.10:6443 not accepting connections yet".into(),
            };
            assert_eq!(code_of(&err), "apprafter::cluster::k3s_not_ready");
            assert_eq!(
                err.to_string(),
                "the kubeconfig of target `prod` did not become reachable within 300s (30 \
                 attempts); last error: apiserver 203.0.113.10:6443 not accepting connections yet"
            );
            let help = help_of(&err);
            for part in [
                stopped,
                "the server of target `prod` exists, and Hetzner bills for it",
                "cluster-bootstrap did not run",
                again,
                "it finds the server it provisioned instead of creating another",
                goes_on,
                "`apprafter doctor --target prod`",
            ] {
                assert!(help.contains(part), "{by:?}: {part:?} missing: {help}");
            }
        }
    }

    #[test]
    fn a_declined_new_age_key_says_nothing_was_fetched_and_names_both_ways_on() {
        let err = CliError::NewAgeKeyDeclined {
            path: "/k/age.key".into(),
            target: "prod".into(),
        };
        assert_eq!(code_of(&err), "apprafter::secrets::new_key_declined");
        assert_eq!(
            err.to_string(),
            "no new age key was created at /k/age.key, so the kubeconfig of target `prod` was \
             not fetched"
        );
        let help = help_of(&err);
        for part in [
            "Nothing was created, fetched or written.",
            "`APPRAFTER_AGE_KEY`",
            "`apprafter kubeconfig --refresh --target prod`",
            "`--yes`",
        ] {
            assert!(help.contains(part), "{part:?} missing: {help}");
        }
        assert!(!help.contains("file an issue"), "{help}");
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
