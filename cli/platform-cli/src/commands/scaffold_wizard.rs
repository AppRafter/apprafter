// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Interactive scaffold prompts shared by two callers:
//!
//!   * `apprafter app add` step 0 — when `<cwd>/apprafter/
//!     Application.cue` is missing AND stdio is а TTY, runs
//!     `run_step_zero(cwd, suggested_name)`: confirm → runtime
//!     pick → final confirm → generate.
//!   * `apprafter app scaffold` standalone — when detection
//!     is inconclusive AND stdio is а TTY, falls back to
//!     `pick_runtime_interactive(&detections)` instead of
//!     erroring с pointer к `--runtime`.
//!
//! Pure helpers `build_runtime_select_options` и
//! `decide_scaffold_step` ship the data shape + decision tree
//! tests cover; the `inquire::Select`/`Confirm` prompts are
//! integration-thin over those pure surfaces.

use std::path::Path;

use cli_core::{CliError, Result};
use inquire::validator::Validation;
use inquire::{Confirm, InquireError, Select, Text};

use crate::commands::app_scaffold::{defaults_for, scaffold, ScaffoldOpts, DEFAULT_NAMESPACE};
use crate::commands::runtime_detect::{detect_runtimes, Confidence, Detection, Runtime};

/// All twelve runtimes, в the order they appear в the
/// `inquire::Select` list. Order matches plan.md §1.79b
/// runtime detection table (Node-likes first, then Python,
/// then compiled, then Docker, then Blank). Stable so tests
/// can pin specific indices.
const ALL_RUNTIMES: [Runtime; 12] = [
    Runtime::Bun,
    Runtime::NodePnpm,
    Runtime::NodeYarn,
    Runtime::NodeNpm,
    Runtime::PythonPoetry,
    Runtime::PythonUv,
    Runtime::PythonPipenv,
    Runtime::PythonPip,
    Runtime::Rust,
    Runtime::Go,
    Runtime::Docker,
    Runtime::Blank,
];

/// Decision tree for `apprafter app add` step 0 — pure
/// function на the inputs (file existence + wizard mode +
/// `--scaffold` flag) for exhaustive testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldDecision {
    /// `<cwd>/apprafter/Application.cue` already exists.
    /// Skip step 0 entirely; existing flow registers the
    /// app as-is.
    Skip,
    /// TTY mode — drop into the full `run_step_zero` wizard
    /// (Confirm → runtime Select → Confirm → scaffold).
    Interactive,
    /// `--no-interactive` + `--scaffold` — auto-generate
    /// using detection's single High match. Fails если
    /// detection is inconclusive (operator should run
    /// `apprafter app scaffold --runtime <slug>` first).
    NonInteractive,
    /// Neither TTY nor `--scaffold` — refuse с hint к
    /// `apprafter app scaffold`. Keeps non-interactive
    /// flows из silently scaffolding behind operators'
    /// backs.
    Refuse,
}

pub fn decide_scaffold_step(
    scaffold_target_exists: bool,
    use_wizard: bool,
    scaffold_flag: bool,
) -> ScaffoldDecision {
    if scaffold_target_exists {
        return ScaffoldDecision::Skip;
    }
    if use_wizard {
        return ScaffoldDecision::Interactive;
    }
    if scaffold_flag {
        return ScaffoldDecision::NonInteractive;
    }
    ScaffoldDecision::Refuse
}

/// Build the `(labels, runtimes, default_idx)` triple that
/// `pick_runtime_interactive` feeds к `inquire::Select`.
/// Pure — tests cover the label format + default cursor
/// calculation without spawning а terminal.
///
/// Labels render as `"<slug>"` для undetected runtimes и
/// `"<slug> (detected via <marker>)"` для detected entries.
/// Default cursor lands on the first High-confidence
/// detection's runtime; ties broken by `ALL_RUNTIMES` order
/// (Node-likes first). Falls back к the first detection of
/// any confidence, then к Blank (last index) when nothing
/// matched.
pub fn build_runtime_select_options(
    detections: &[Detection],
) -> (Vec<String>, [Runtime; 12], usize) {
    let labels: Vec<String> = ALL_RUNTIMES
        .iter()
        .map(|r| match detections.iter().find(|d| d.runtime == *r) {
            Some(d) => match d.marker.as_deref() {
                Some(marker) if !marker.is_empty() => {
                    format!("{} (detected via {marker})", r.slug())
                }
                _ => format!("{} (detected)", r.slug()),
            },
            None => r.slug().to_string(),
        })
        .collect();

    // Default cursor: walk ALL_RUNTIMES в order, pick the
    // first runtime that has а High detection. Fallback: any
    // detection. Fallback: Blank (last index). Walking
    // ALL_RUNTIMES (not the detections array) means ties
    // resolve by display order, not detection order — operator
    // running scaffold з repo root с bun.lock + Cargo.toml
    // lands on Bun (idx 0) regardless of which marker the
    // detector found first.
    let default_idx = ALL_RUNTIMES
        .iter()
        .position(|r| {
            detections
                .iter()
                .any(|d| d.runtime == *r && d.confidence == Confidence::High)
        })
        .or_else(|| {
            ALL_RUNTIMES
                .iter()
                .position(|r| detections.iter().any(|d| d.runtime == *r))
        })
        .unwrap_or(ALL_RUNTIMES.len() - 1);

    (labels, ALL_RUNTIMES, default_idx)
}

/// Drop into а Select prompt; return the picked `Runtime`.
/// Wraps `inquire::Select` over `build_runtime_select_options`'
/// pure surface.
pub fn pick_runtime_interactive(detections: &[Detection]) -> Result<Runtime> {
    let (labels, runtimes, default_idx) = build_runtime_select_options(detections);
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

    let selected = Select::new("Runtime preset:", label_refs)
        .with_starting_cursor(default_idx)
        .prompt()
        .map_err(prompt_err)?;

    let idx = labels
        .iter()
        .position(|l| l.as_str() == selected)
        .ok_or_else(|| CliError::Other("internal: selected label not found in options".into()))?;
    Ok(runtimes[idx])
}

/// Values that step 0 settled on; the caller (`app::add`)
/// uses these к pre-fill the outer wizard's «name» и
/// «namespace» prompts so the operator doesn't restate the
/// same answer twice. Without this propagation, scaffold's
/// `metadata.namespace` would race the wizard's `--namespace`
/// — see walk-fix post-Part-3b: scaffold wrote `apprafter`
/// while operator later picked `procvue` в the wizard,
/// leaving the manifest's namespace inconsistent с Argo CD's
/// `destination.namespace`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepZeroOutput {
    pub name: String,
    pub namespace: String,
}

/// Full step-0 wizard surface called by `commands::app::add`
/// когда `<cwd>/apprafter/Application.cue` is missing AND
/// stdio is а TTY. Sequence:
///
///   1. Confirm: «No `apprafter/Application.cue` found.
///      Generate one?» (default Yes). No → error с pointer
///      к standalone `apprafter app scaffold`.
///   2. Run detection и render а Select с all 12 runtimes;
///      default cursor on the first High match.
///   3. Text prompt: «AppRafter app name?» (default =
///      sanitised cwd basename). DNS-1123 validated inline.
///   4. Text prompt: «Destination namespace?» (default =
///      `apprafter`). DNS-1123 validated inline.
///   5. Final Confirm с summary (runtime + name + namespace).
///      No → «scaffold cancelled».
///   6. Invoke `commands::app_scaffold::scaffold(opts)` to
///      write the file и update `.gitignore`.
///
/// Returns the `(name, namespace)` actually used so the
/// caller can propagate к the outer wizard's prompts.
pub fn run_step_zero(cwd: &Path, suggested_name: &str) -> Result<StepZeroOutput> {
    let proceed = Confirm::new("No `apprafter/Application.cue` found. Generate one?")
        .with_default(true)
        .prompt()
        .map_err(prompt_err)?;
    if !proceed {
        return Err(CliError::Other(
            "Cannot register an app without `apprafter/Application.cue`. Run \
             `apprafter app scaffold` manually, or rerun `apprafter app add` \
             and choose Yes when prompted."
                .into(),
        ));
    }

    let detections = detect_runtimes(cwd);
    let runtime = pick_runtime_interactive(&detections)?;
    let defaults = defaults_for(runtime);

    let name = prompt_dns_1123_text("AppRafter app name (metadata.name)", suggested_name)?;
    let namespace = prompt_dns_1123_text(
        "Destination namespace (metadata.namespace)",
        DEFAULT_NAMESPACE,
    )?;

    eprintln!();
    eprintln!("→ Will generate apprafter/Application.cue with:");
    eprintln!("    runtime:   {} ({})", defaults.display, runtime.slug());
    eprintln!("    name:      {name}");
    eprintln!("    namespace: {namespace}");
    eprintln!("    port:      {}", defaults.primary_port);
    eprintln!();

    let confirm = Confirm::new("Generate now?")
        .with_default(true)
        .prompt()
        .map_err(prompt_err)?;
    if !confirm {
        return Err(CliError::Other("scaffold cancelled".into()));
    }

    let opts = ScaffoldOpts {
        runtime: Some(runtime),
        name: Some(name.clone()),
        namespace: Some(namespace.clone()),
        path: cwd.to_path_buf(),
        force: false,
    };
    scaffold(opts)?;
    eprintln!();
    Ok(StepZeroOutput { name, namespace })
}

fn prompt_dns_1123_text(label: &str, default: &str) -> Result<String> {
    let value = Text::new(label)
        .with_default(default)
        .with_validator(dns_1123_validator)
        .prompt()
        .map_err(prompt_err)?;
    Ok(value.trim().to_string())
}

fn dns_1123_validator(value: &str) -> std::result::Result<Validation, inquire::CustomUserError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(Validation::Invalid("must not be empty".into()));
    }
    if trimmed.len() > 63 {
        return Ok(Validation::Invalid("must be 1-63 characters".into()));
    }
    let ok = trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !trimmed.starts_with('-')
        && !trimmed.ends_with('-');
    if !ok {
        return Ok(Validation::Invalid(
            "DNS-1123 only: lower-case [a-z0-9-], no leading/trailing dash".into(),
        ));
    }
    Ok(Validation::Valid)
}

fn prompt_err(e: InquireError) -> CliError {
    match e {
        InquireError::OperationCanceled | InquireError::OperationInterrupted => {
            CliError::Other("wizard cancelled".into())
        }
        _ => CliError::Other(format!("wizard prompt failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(runtime: Runtime, confidence: Confidence, marker: &str) -> Detection {
        Detection {
            runtime,
            confidence,
            marker: Some(marker.to_string()),
        }
    }

    #[test]
    fn decide_scaffold_step_skips_when_target_exists() {
        assert_eq!(
            decide_scaffold_step(true, true, true),
            ScaffoldDecision::Skip
        );
        assert_eq!(
            decide_scaffold_step(true, false, false),
            ScaffoldDecision::Skip
        );
        assert_eq!(
            decide_scaffold_step(true, true, false),
            ScaffoldDecision::Skip
        );
    }

    #[test]
    fn decide_scaffold_step_routes_interactive_when_wizard_active() {
        // TTY mode wins regardless of --scaffold (wizard
        // prompts naturally; scaffold flag is а non-
        // interactive convenience).
        assert_eq!(
            decide_scaffold_step(false, true, false),
            ScaffoldDecision::Interactive
        );
        assert_eq!(
            decide_scaffold_step(false, true, true),
            ScaffoldDecision::Interactive
        );
    }

    #[test]
    fn decide_scaffold_step_routes_non_interactive_when_scaffold_flag_set() {
        assert_eq!(
            decide_scaffold_step(false, false, true),
            ScaffoldDecision::NonInteractive
        );
    }

    #[test]
    fn decide_scaffold_step_refuses_when_neither_wizard_nor_scaffold_flag() {
        // The default non-TTY case — silent scaffolding
        // would surprise operators в CI pipelines. Force
        // them to be explicit about wanting а scaffold.
        assert_eq!(
            decide_scaffold_step(false, false, false),
            ScaffoldDecision::Refuse
        );
    }

    #[test]
    fn build_runtime_select_options_lists_all_twelve_with_default_blank_when_no_detections() {
        let (labels, runtimes, default_idx) = build_runtime_select_options(&[]);
        assert_eq!(labels.len(), 12);
        assert_eq!(runtimes.len(), 12);
        assert_eq!(default_idx, 11, "default should fall back to Blank");
        // Spot-check: undetected entries render as bare slug.
        assert_eq!(labels[0], "bun");
        assert_eq!(labels[8], "rust");
        assert_eq!(labels[11], "blank");
    }

    #[test]
    fn build_runtime_select_options_marks_detected_runtimes_with_marker() {
        let detections = [detection(Runtime::Bun, Confidence::High, "bun.lock")];
        let (labels, _, default_idx) = build_runtime_select_options(&detections);
        assert_eq!(labels[0], "bun (detected via bun.lock)");
        assert_eq!(default_idx, 0, "default should land on Bun (index 0)");
        // Non-detected entries stay bare.
        assert_eq!(labels[8], "rust");
    }

    #[test]
    fn build_runtime_select_options_default_idx_picks_first_high_in_array_order() {
        // bun (idx 0) и rust (idx 8) both High — default
        // lands on the earlier index per ALL_RUNTIMES order.
        let detections = [
            detection(Runtime::Rust, Confidence::High, "Cargo.toml"),
            detection(Runtime::Bun, Confidence::High, "bun.lock"),
        ];
        let (_, _, default_idx) = build_runtime_select_options(&detections);
        assert_eq!(default_idx, 0, "Bun precedes Rust in ALL_RUNTIMES order");
    }

    #[test]
    fn build_runtime_select_options_falls_back_to_medium_when_no_high() {
        // Bare package.json — only а Medium detection.
        // Default cursor still lands on it (Node-NPM is idx
        // 3) rather than Blank.
        let detections = [detection(
            Runtime::NodeNpm,
            Confidence::Medium,
            "package.json",
        )];
        let (labels, _, default_idx) = build_runtime_select_options(&detections);
        assert_eq!(default_idx, 3);
        assert_eq!(labels[3], "node-npm (detected via package.json)");
    }

    #[test]
    fn build_runtime_select_options_handles_detection_without_marker() {
        // Blank/Fallback has marker = None; label renders
        // как «<slug> (detected)» without the «via X» tail.
        let detections = [Detection {
            runtime: Runtime::Blank,
            confidence: Confidence::Fallback,
            marker: None,
        }];
        let (labels, _, default_idx) = build_runtime_select_options(&detections);
        assert_eq!(labels[11], "blank (detected)");
        // Default lands on Blank even though it's Fallback —
        // it's the only detection AND there's no High.
        assert_eq!(default_idx, 11);
    }
}
