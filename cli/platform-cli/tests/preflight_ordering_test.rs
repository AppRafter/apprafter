// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The ordering D11 recorded, asserted against the source.
//!
//! The defect was never "the error is untyped". It was that the check
//! ran *after* the part of the command that costs something:
//!
//! ```text
//! $ apprafter backup list
//! > Backup passphrase: ********
//! Error: apprafter::cli::other
//!   × spawn restic: No such file or directory (os error 2)
//! ```
//!
//! A secret typed into a command that could not have worked. The
//! sharpest instance was `restore --reprovision`, which gates the
//! passphrase *deliberately* — with a comment explaining that a bad
//! passphrase must not leave a re-provisioned cluster half-restored —
//! and did not gate the binary, so a missing `restic` cost a paid,
//! running Hetzner cluster before anything noticed.
//!
//! # Why this reads source instead of running the commands
//!
//! The honest runtime test is "empty the `PATH`, call the function,
//! assert it returns before prompting". That needs a process-global
//! mutation of `PATH` in a suite that runs in parallel, which is both
//! racy and `unsafe` on the 2024 edition. Reading the source answers
//! the same question deterministically: within each guarded function,
//! the preflight must textually precede every hazard.
//!
//! The failure this actually guards is a regression by a later author —
//! someone adds a prompt, a kubeconfig or a provider call above the
//! preflight and reintroduces the bug in a new command. That is a
//! source-order mistake, so a source-order assertion catches it at the
//! right moment.
//!
//! Its limit is worth stating: it proves textual order inside one
//! function, not that a callee prompts. A prompt hidden inside a helper
//! called before the preflight would pass. The hazard list below is
//! therefore written against what these functions actually call.

/// One function that must check its binaries before it does anything
/// expensive, and the calls that count as expensive in it.
struct Guarded {
    file: &'static str,
    func: &'static str,
    /// Substrings that must not appear before the preflight. Chosen
    /// per-function against what it really calls, not as a generic list
    /// — a generic list is either too loose to catch anything or so
    /// tight it fires on a doc comment.
    hazards: &'static [&'static str],
    /// The incident, so a later reader can judge the entry.
    because: &'static str,
}

const GUARDED: &[Guarded] = &[
    Guarded {
        file: "src/commands/restore.rs",
        func: "run_restore",
        hazards: &[
            "resolve_operator_s3_creds(",
            "inquire::",
            "ensure_kubeconfig_tempfile(",
        ],
        because: "the passphrase gate was deliberate and the binary gate was \
                  missing, so --reprovision spent a billable cluster first",
    },
    Guarded {
        file: "src/commands/backup.rs",
        func: "run_backup_list",
        hazards: &["backup_passphrase_or_error(", "inquire::"],
        because: "the reported transcript: passphrase prompt, then spawn restic \
                  failed",
    },
    Guarded {
        file: "src/commands/backup.rs",
        func: "run_backup",
        hazards: &[
            "backup_passphrase_or_error(",
            "inquire::",
            "ensure_kubeconfig_tempfile(",
        ],
        because: "prompt, then kubeconfig, then kubectl, then restic — an \
                  operator with no cluster typed a secret and was then told \
                  there was no cluster",
    },
    Guarded {
        file: "src/commands/backup.rs",
        func: "run_backup_prune",
        hazards: &[
            "kubeconfig_if_cluster_needed(",
            "resolve_operator_s3_creds(",
        ],
        because: "round-trips to the cluster before spawning restic, on a verb \
                  built to work when the cluster is gone",
    },
    Guarded {
        file: "src/commands/backup.rs",
        func: "run_backup_check",
        hazards: &[
            "kubeconfig_if_cluster_needed(",
            "resolve_operator_s3_creds(",
        ],
        because: "same, and `check --repo` is explicitly designed to run with no \
                  cluster at all",
    },
    Guarded {
        file: "src/commands/backup.rs",
        func: "run_backup_unlock",
        hazards: &[
            "kubeconfig_if_cluster_needed(",
            "resolve_operator_s3_creds(",
        ],
        because: "same — these are the outage commands, where a missing binary \
                  matters more, not less",
    },
    Guarded {
        file: "src/commands/bootstrap_all.rs",
        func: "run",
        hazards: &["apply::run("],
        because: "phase 1/3 creates billable resources and phase 3/3 is the first \
                  code to need helm",
    },
    Guarded {
        file: "src/commands/repo_creds.rs",
        func: "add",
        hazards: &[
            "should_use_wizard(",
            "resolve_token(",
            "ensure_kubeconfig_tempfile(",
        ],
        because: "the wizard collects a production PAT over four prompts and only \
                  then discovers there is no kubectl or no cluster",
    },
];

/// The body of `pub fn <name>(` in `source`, from the signature to the
/// first column-0 closing brace after it.
fn body_of<'a>(source: &'a str, func: &str) -> Option<&'a str> {
    let sig = format!("\npub fn {func}(");
    let start = source.find(&sig)? + 1;
    let rest = &source[start..];
    let end = rest.find("\n}\n").map(|i| i + 2).unwrap_or(rest.len());
    Some(&rest[..end])
}

fn read(file: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn every_guarded_command_preflights_before_it_spends_anything() {
    for g in GUARDED {
        let source = read(g.file);
        let body = body_of(&source, g.func)
            .unwrap_or_else(|| panic!("`pub fn {}` not found in {}", g.func, g.file));

        let preflight = body.find("preflight_tool").unwrap_or_else(|| {
            panic!(
                "{}::{} calls no preflight. It spawns an external binary, so a \
                 missing one surfaces as a raw `os error 2` after the command has \
                 already done work. Why this one is guarded: {}",
                g.file, g.func, g.because
            )
        });

        for hazard in g.hazards {
            if let Some(at) = body.find(hazard) {
                assert!(
                    preflight < at,
                    "{}::{} calls `{}` at byte {} BEFORE its preflight at byte {}. \
                     That is the D11 defect reintroduced: the expensive half runs \
                     first and the cheap check that would have stopped it runs \
                     after. Why this one is guarded: {}",
                    g.file,
                    g.func,
                    hazard,
                    at,
                    preflight,
                    g.because
                );
            }
        }
    }
}

#[test]
fn the_guard_list_is_not_vacuous() {
    // A hazard that no longer appears in its function makes that entry
    // silently stop guarding anything — the assertion above skips a
    // hazard it cannot find, deliberately, so that a refactor does not
    // fail the build for the wrong reason. This test is the other half:
    // every guarded function must still contain at least one hazard, or
    // the entry has rotted and needs re-deriving rather than trusting.
    for g in GUARDED {
        let source = read(g.file);
        let body = body_of(&source, g.func).expect("function present");
        let live = g.hazards.iter().filter(|h| body.contains(*h)).count();
        assert!(
            live > 0,
            "{}::{} no longer contains ANY of its recorded hazards {:?}. The entry \
             is now vacuous — it would pass whatever the ordering is. Re-derive the \
             hazards from what the function calls today.",
            g.file,
            g.func,
            g.hazards
        );
    }
}

#[test]
fn every_entry_records_the_incident_that_earned_it() {
    for g in GUARDED {
        assert!(
            g.because.len() > 40,
            "{}::{} needs the incident that earned it, not a label",
            g.file,
            g.func
        );
    }
}
