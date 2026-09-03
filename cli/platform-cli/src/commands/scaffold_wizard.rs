// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Interactive scaffold prompts shared by two callers:
//!
//!   * `apprafter app add` step 0 — when `<cwd>/apprafter/
//!     Application.cue` is missing AND stdio is a TTY, runs
//!     `run_step_zero(cwd, suggested_name)`: confirm → runtime
//!     pick → final confirm → generate.
//!   * `apprafter app scaffold` standalone — when detection
//!     is inconclusive AND stdio is a TTY, falls back to
//!     `pick_runtime_interactive(&detections)` instead of
//!     erroring with a pointer to `--runtime`.
//!
//! Pure helpers `build_runtime_select_options` and
//! `decide_scaffold_step` ship the data shape + decision tree
//! tests cover; the `inquire::Select`/`Confirm` prompts are
//! integration-thin over those pure surfaces.

use std::path::Path;

use cli_core::{CliError, Result};
use inquire::validator::Validation;
use inquire::{Confirm, InquireError, Select, Text};

use crate::commands::app_scaffold::{defaults_for, scaffold, ScaffoldOpts, DEFAULT_NAMESPACE};
use crate::commands::runtime_detect::{detect_runtimes, Confidence, Detection, Runtime};

/// All twelve runtimes, in the order they appear in the
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
/// function on the inputs (file existence + wizard mode +
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
    /// using detection's single High match. Fails if
    /// detection is inconclusive (operator should run
    /// `apprafter app scaffold --runtime <slug>` first).
    NonInteractive,
    /// Neither TTY nor `--scaffold` — refuse with a hint to
    /// `apprafter app scaffold`. Keeps non-interactive
    /// flows from silently scaffolding behind operators'
    /// backs.
    Refuse,
}

pub fn decide_scaffold_step(
    scaffold_target_exists: bool,
    use_wizard: bool,
    scaffold_flag: bool,
    explicit_git_url: bool,
) -> ScaffoldDecision {
    // An explicit `<git-url>` names where the manifest lives, and
    // Argo CD renders it from there. Step 0 bridges a fresh LOCAL
    // checkout to a registered app; there is nothing local to bridge
    // here, and requiring a cwd manifest contradicts the argument's
    // own help — "Required when cwd is not a git repo".
    if explicit_git_url {
        return ScaffoldDecision::Skip;
    }
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
/// `pick_runtime_interactive` feeds to `inquire::Select`.
/// Pure — tests cover the label format + default cursor
/// calculation without spawning a terminal.
///
/// Labels render as `"<slug>"` for undetected runtimes and
/// `"<slug> (detected via <marker>)"` for detected entries.
/// Default cursor lands on the first High-confidence
/// detection's runtime; ties broken by `ALL_RUNTIMES` order
/// (Node-likes first). Falls back to the first detection of
/// any confidence, then to Blank (last index) when nothing
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

    // Default cursor: walk ALL_RUNTIMES in order, pick the
    // first runtime that has a High detection. Fallback: any
    // detection. Fallback: Blank (last index). Walking
    // ALL_RUNTIMES (not the detections array) means ties
    // resolve by display order, not detection order — operator
    // running scaffold from repo root with bun.lock + Cargo.toml
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

/// Drop into a Select prompt; return the picked `Runtime`.
/// Wraps `inquire::Select` over `build_runtime_select_options`'
/// pure surface.
pub fn pick_runtime_interactive(detections: &[Detection]) -> Result<Runtime> {
    let (labels, runtimes, default_idx) = build_runtime_select_options(detections);
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

    let selected = Select::new("Runtime preset:", label_refs)
        .with_starting_cursor(default_idx)
        .prompt()
        .map_err(prompt_err)?;

    resolve_runtime_from_label(&labels, &runtimes, selected)
}

/// Map the label the operator picked back to its `Runtime`.
///
/// `inquire::Select` hands back the chosen *string*, so the label
/// list is the only index we have — which is why
/// `build_runtime_select_options` must emit one label per runtime,
/// in the same order, and why duplicate labels would silently
/// resolve to the wrong runtime. Pure — extracted from
/// `pick_runtime_interactive` so the mapping (and its
/// unknown-label guard) is testable without a terminal.
fn resolve_runtime_from_label(
    labels: &[String],
    runtimes: &[Runtime; 12],
    selected: &str,
) -> Result<Runtime> {
    let idx = labels
        .iter()
        .position(|l| l.as_str() == selected)
        .ok_or_else(|| CliError::Other("internal: selected label not found in options".into()))?;
    Ok(runtimes[idx])
}

/// Values that step 0 settled on; the caller (`app::add`)
/// uses these to pre-fill the outer wizard's "name" and
/// "namespace" prompts so the operator doesn't restate the
/// same answer twice. Without this propagation, scaffold's
/// `metadata.namespace` would race the wizard's `--namespace`
/// — see walk-fix post-Part-3b: scaffold wrote `apprafter`
/// while operator later picked `procvue` in the wizard,
/// leaving the manifest's namespace inconsistent with Argo CD's
/// `destination.namespace`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepZeroOutput {
    pub name: String,
    pub namespace: String,
}

/// Full step-0 wizard surface called by `commands::app::add`
/// when `<cwd>/apprafter/Application.cue` is missing AND
/// stdio is a TTY. Sequence:
///
///   1. Confirm: "No `apprafter/Application.cue` found.
///      Generate one?" (default Yes). No → error with a pointer
///      to standalone `apprafter app scaffold`.
///   2. Run detection and render a Select with all 12 runtimes;
///      default cursor on the first High match.
///   3. Text prompt: "AppRafter app name?" (default =
///      sanitised cwd basename). DNS-1123 validated inline.
///   4. Text prompt: "Destination namespace?" (default =
///      `apprafter`). DNS-1123 validated inline.
///   5. Final Confirm with summary (runtime + name + namespace).
///      No → "scaffold cancelled".
///   6. Invoke `commands::app_scaffold::scaffold(opts)` to
///      write the file and update `.gitignore`.
///
/// Returns the `(name, namespace)` actually used so the
/// caller can propagate to the outer wizard's prompts.
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
        needs: Vec::new(),
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
    match check_dns_1123(value) {
        Ok(()) => Ok(Validation::Valid),
        Err(msg) => Ok(Validation::Invalid(msg.into())),
    }
}

/// DNS-1123 label rule for the name / namespace prompts, as a
/// plain `Result` instead of an `inquire::Validation`.
///
/// Both values become Kubernetes object names, so a value that
/// slips through here is rejected much later by the apiserver (or
/// by the admission webhook) with a message that no longer points
/// at the prompt that produced it. Pure — extracted from
/// `dns_1123_validator` so every rejection reason is testable
/// without a terminal.
fn check_dns_1123(value: &str) -> std::result::Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("must not be empty".to_string());
    }
    if trimmed.len() > 63 {
        return Err("must be 1-63 characters".to_string());
    }
    let ok = trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !trimmed.starts_with('-')
        && !trimmed.ends_with('-');
    if !ok {
        return Err("DNS-1123 only: lower-case [a-z0-9-], no leading/trailing dash".to_string());
    }
    Ok(())
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
            decide_scaffold_step(true, true, true, false),
            ScaffoldDecision::Skip
        );
        assert_eq!(
            decide_scaffold_step(true, false, false, false),
            ScaffoldDecision::Skip
        );
        assert_eq!(
            decide_scaffold_step(true, true, false, false),
            ScaffoldDecision::Skip
        );
    }

    #[test]
    fn decide_scaffold_step_routes_interactive_when_wizard_active() {
        // TTY mode wins regardless of --scaffold (wizard
        // prompts naturally; scaffold flag is a non-
        // interactive convenience).
        assert_eq!(
            decide_scaffold_step(false, true, false, false),
            ScaffoldDecision::Interactive
        );
        assert_eq!(
            decide_scaffold_step(false, true, true, false),
            ScaffoldDecision::Interactive
        );
    }

    #[test]
    fn decide_scaffold_step_routes_non_interactive_when_scaffold_flag_set() {
        assert_eq!(
            decide_scaffold_step(false, false, true, false),
            ScaffoldDecision::NonInteractive
        );
    }

    #[test]
    fn decide_scaffold_step_refuses_when_neither_wizard_nor_scaffold_flag() {
        // The default non-TTY case — silent scaffolding
        // would surprise operators in CI pipelines. Force
        // them to be explicit about wanting a scaffold.
        assert_eq!(
            decide_scaffold_step(false, false, false, false),
            ScaffoldDecision::Refuse
        );
    }

    #[test]
    fn decide_scaffold_step_skips_when_an_explicit_git_url_was_given() {
        // `app add <git-url>` names the repository that holds the
        // manifest. Step 0 exists to bridge a fresh LOCAL checkout
        // to a registered app; with an explicit URL there is nothing
        // local to bridge, and demanding a cwd manifest contradicts
        // the argument's own help ("Required when cwd is not a git
        // repo"). The documented workaround was cloning a repository
        // the operator does not otherwise need.
        for use_wizard in [true, false] {
            for scaffold_flag in [true, false] {
                assert_eq!(
                    decide_scaffold_step(false, use_wizard, scaffold_flag, true),
                    ScaffoldDecision::Skip,
                    "explicit git url must skip step 0 \
                     (wizard={use_wizard}, scaffold={scaffold_flag})"
                );
            }
        }
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
        // bun (idx 0) and rust (idx 8) both High — default
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
        // Bare package.json — only a Medium detection.
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
        // as "<slug> (detected)" without the "via X" tail.
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

    /// A detector that reports a match but hands back an empty
    /// marker string must still render as a plain "(detected)" —
    /// `"bun (detected via )"` with a dangling preposition looks
    /// like a truncation bug to the operator staring at the picker.
    #[test]
    fn build_runtime_select_options_treats_an_empty_marker_like_a_missing_one() {
        let detections = [Detection {
            runtime: Runtime::Bun,
            confidence: Confidence::High,
            marker: Some(String::new()),
        }];
        let (labels, _, default_idx) = build_runtime_select_options(&detections);
        assert_eq!(labels[0], "bun (detected)");
        assert_eq!(default_idx, 0);
    }

    // ---------------------------------------------------------------
    // Label → Runtime resolution.
    //
    // `inquire::Select` returns the chosen STRING, so the label
    // list is the only index back to a `Runtime`. Everything the
    // scaffold then writes (template, port, start command) hangs
    // off that lookup being exact.
    // ---------------------------------------------------------------

    /// Every label the picker can display resolves back to the
    /// runtime that produced it — including the decorated
    /// "(detected via …)" labels, which is where an off-by-one or a
    /// prefix match would show up. A wrong answer here scaffolds a
    /// Go manifest for a Bun repo without any error.
    #[test]
    fn resolve_runtime_from_label_round_trips_every_offered_label() {
        // A detection on a middle entry so the list mixes decorated
        // and bare labels.
        let detections = [detection(Runtime::PythonUv, Confidence::High, "uv.lock")];
        let (labels, runtimes, _) = build_runtime_select_options(&detections);
        for (i, label) in labels.iter().enumerate() {
            let got = resolve_runtime_from_label(&labels, &runtimes, label)
                .expect("a label the picker offered must resolve");
            assert_eq!(
                got, runtimes[i],
                "label {label:?} resolved to the wrong runtime"
            );
        }
    }

    /// A label that isn't in the offered list is an internal
    /// inconsistency, not operator input — it must surface as an
    /// error rather than resolving to whatever sits at index 0.
    #[test]
    fn resolve_runtime_from_label_errors_on_a_label_that_was_never_offered() {
        let (labels, runtimes, _) = build_runtime_select_options(&[]);
        let err = resolve_runtime_from_label(&labels, &runtimes, "cobol")
            .expect_err("an unoffered label must not resolve");
        assert!(err.to_string().contains("not found in options"), "{err}");
    }

    // ---------------------------------------------------------------
    // DNS-1123 rule for the name / namespace prompts.
    // ---------------------------------------------------------------

    /// The shapes Kubernetes actually accepts as an object name.
    /// Digits, internal dashes and the 63-character boundary all
    /// have to pass — rejecting them would block legitimate names
    /// at the prompt with no way around it.
    #[test]
    fn check_dns_1123_accepts_legal_kubernetes_object_names() {
        for ok in ["a", "web", "my-app-2", "0", "app2you", &"a".repeat(63)] {
            assert!(check_dns_1123(ok).is_ok(), "{ok:?} should be accepted");
        }
        // Surrounding whitespace is trimmed before judging, matching
        // what `prompt_dns_1123_text` stores.
        assert!(check_dns_1123("  web  ").is_ok());
    }

    /// Each rejection reason is distinct so the prompt can tell the
    /// operator what to fix. Anything let through here reaches the
    /// apiserver, which rejects it much later with a message that no
    /// longer points at this prompt.
    #[test]
    fn check_dns_1123_rejects_each_illegal_shape_with_its_own_reason() {
        assert_eq!(check_dns_1123("").unwrap_err(), "must not be empty");
        // Whitespace-only is empty after trimming, not a character
        // violation.
        assert_eq!(check_dns_1123("   ").unwrap_err(), "must not be empty");
        assert_eq!(
            check_dns_1123(&"a".repeat(64)).unwrap_err(),
            "must be 1-63 characters"
        );

        let charset = "DNS-1123 only: lower-case [a-z0-9-], no leading/trailing dash";
        for bad in [
            "MyApp",  // upper-case
            "my_app", // underscore
            "my.app", // dot
            "my app", // inner space
            "-web",   // leading dash
            "web-",   // trailing dash
            "wéb",    // non-ASCII
        ] {
            assert_eq!(
                check_dns_1123(bad).unwrap_err(),
                charset,
                "{bad:?} should be rejected on the charset rule"
            );
        }
    }

    /// The `inquire` adapter must not invert the verdict: a value
    /// `check_dns_1123` accepts has to reach `Validation::Valid`,
    /// and a rejected one has to carry the reason through as the
    /// custom message the prompt shows. A swapped pair here would
    /// block every legal name while waving illegal ones through.
    #[test]
    fn dns_1123_validator_forwards_the_verdict_and_the_reason_to_inquire() {
        match dns_1123_validator("my-app").expect("validator itself must not fail") {
            Validation::Valid => {}
            Validation::Invalid(msg) => panic!("legal name was rejected: {msg:?}"),
        }
        match dns_1123_validator("My_App").expect("validator itself must not fail") {
            Validation::Invalid(inquire::validator::ErrorMessage::Custom(msg)) => {
                assert_eq!(msg, check_dns_1123("My_App").unwrap_err());
            }
            other => panic!("illegal name must carry a custom reason, got {other:?}"),
        }
    }

    /// Esc / Ctrl-C at any step-0 prompt is a user decision, not a
    /// crash: it surfaces as a plain "cancelled" line. Other
    /// failures keep the underlying inquire error so they stay
    /// diagnosable.
    #[test]
    fn prompt_err_reports_user_cancellation_separately_from_real_failures() {
        assert_eq!(
            prompt_err(InquireError::OperationCanceled).to_string(),
            "wizard cancelled"
        );
        assert_eq!(
            prompt_err(InquireError::OperationInterrupted).to_string(),
            "wizard cancelled"
        );
        let other = prompt_err(InquireError::NotTTY).to_string();
        assert!(other.starts_with("wizard prompt failed:"), "{other}");
    }
}
