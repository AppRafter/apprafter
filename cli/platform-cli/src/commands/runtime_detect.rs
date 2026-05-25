// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Runtime auto-detection from filesystem markers. Track
//! B.1.79b Part 2.
//!
//! Pure helpers consumed by Part 3's `apprafter app scaffold`
//! и the wizard step в `apprafter app add` that asks
//! «No `apprafter/Application.cue` found — generate one?».
//! Detection here is filesystem-only (no shell-out, no
//! network), so tests drive every branch с `tempfile::tempdir`
//! fixtures.
//!
//! Marker → runtime mapping per plan.md §1.79b table; the
//! `Confidence` axis tells the caller how to render the prompt:
//!
//!   * `High` — lockfile present, runtime+pin-source both
//!     evident. Auto-select в non-interactive mode unless
//!     multiple Highs collide (then prompt for choice).
//!   * `Medium` — primary marker present но no lockfile (e.g.
//!     bare `package.json` или `requirements.txt`). Still
//!     prompt for confirmation в TTY.
//!   * `Low` — only а weak signal (Dockerfile alone), used as
//!     а last-resort runtime hint before falling back к
//!     blank.
//!   * `Fallback` — no markers at all. `Blank` template is the
//!     only candidate.
//!
//! `detect_runtimes(dir)` returns the full list of matches in
//! marker-evaluation order. Multi-stack monorepos (e.g. а
//! `landing/` директория с bun.lock alongside а sibling
//! `apprafter-operator/` с Cargo.toml when operator runs from
//! repo root) get all matches surfaced; the caller's prompt
//! shows them and picks default alphabetically per plan.md.
//!
//! `#![allow(dead_code)]` at module scope: Part 2 ships the
//! detection primitives as а standalone landable; Part 3
//! (`apprafter app scaffold` + scaffold wizard step в `app
//! add`) wires the consumers. Until then, clippy's
//! `-D dead-code` would reject the public surface даже though
//! it's intentionally future-consumed. Remove the allow
//! once Part 3 lands.
#![allow(dead_code)]

use std::path::Path;

/// One of the runtime presets the scaffold can target. Slug
/// matches the template filename in `cli/templates/
/// application/<slug>.cue.hbs` (delivered в Part 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Runtime {
    Bun,
    NodePnpm,
    NodeYarn,
    NodeNpm,
    PythonPoetry,
    PythonUv,
    PythonPipenv,
    PythonPip,
    Rust,
    Go,
    Docker,
    Blank,
}

impl Runtime {
    /// Stable kebab-case slug used as the template filename
    /// stem AND the `--runtime <slug>` flag value (Part 3
    /// surface).
    pub const fn slug(self) -> &'static str {
        match self {
            Runtime::Bun => "bun",
            Runtime::NodePnpm => "node-pnpm",
            Runtime::NodeYarn => "node-yarn",
            Runtime::NodeNpm => "node-npm",
            Runtime::PythonPoetry => "python-poetry",
            Runtime::PythonUv => "python-uv",
            Runtime::PythonPipenv => "python-pipenv",
            Runtime::PythonPip => "python-pip",
            Runtime::Rust => "rust",
            Runtime::Go => "go",
            Runtime::Docker => "docker",
            Runtime::Blank => "blank",
        }
    }

    /// Human-readable display for the `inquire::Select`
    /// prompt's option labels. Currently same as slug but
    /// reserved for future "Bun · TypeScript runtime" style
    /// expansion without breaking flag values.
    pub const fn display_name(self) -> &'static str {
        self.slug()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    Fallback,
    Low,
    Medium,
    High,
}

/// One detected runtime candidate. `marker` carries the
/// filename (or content excerpt for [tool.X] cases) that
/// triggered the match so the prompt can render «detected via
/// `bun.lock`» — operator skimming the wizard sees why this
/// runtime is suggested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub runtime: Runtime,
    pub confidence: Confidence,
    pub marker: Option<String>,
}

/// Scan `dir` for runtime markers and return all matches.
/// Order is stable: bun → pnpm → yarn → npm-lock → bare
/// package.json → poetry → uv → pipenv → bare requirements
/// → rust → go → docker → blank. Multi-stack monorepos
/// return multiple entries; empty dirs return а single
/// `Blank/Fallback`.
///
/// Pure: never shells out, doesn't write anything. Tests
/// drive с `tempdir()` fixtures.
pub fn detect_runtimes(dir: &Path) -> Vec<Detection> {
    let mut matches = Vec::new();

    // ── Node-like runtimes ─────────────────────────────────
    if file_exists(dir, "bun.lock") || file_exists(dir, "bun.lockb") {
        let marker = if file_exists(dir, "bun.lockb") {
            "bun.lockb"
        } else {
            "bun.lock"
        };
        matches.push(Detection {
            runtime: Runtime::Bun,
            confidence: Confidence::High,
            marker: Some(marker.to_string()),
        });
    } else if file_exists(dir, "pnpm-lock.yaml") {
        matches.push(Detection {
            runtime: Runtime::NodePnpm,
            confidence: Confidence::High,
            marker: Some("pnpm-lock.yaml".to_string()),
        });
    } else if file_exists(dir, "yarn.lock") {
        matches.push(Detection {
            runtime: Runtime::NodeYarn,
            confidence: Confidence::High,
            marker: Some("yarn.lock".to_string()),
        });
    } else if file_exists(dir, "package-lock.json") {
        matches.push(Detection {
            runtime: Runtime::NodeNpm,
            confidence: Confidence::High,
            marker: Some("package-lock.json".to_string()),
        });
    } else if file_exists(dir, "package.json") {
        // Bare package.json — runtime known, package manager
        // not pinned. plan.md routes this к `node-npm`
        // (cheapest path) с Medium confidence so the wizard
        // confirms.
        matches.push(Detection {
            runtime: Runtime::NodeNpm,
            confidence: Confidence::Medium,
            marker: Some("package.json".to_string()),
        });
    }

    // ── Python ────────────────────────────────────────────
    // pyproject.toml content drives the manager selection.
    // We text-grep for the [tool.X] section header rather than
    // pulling in а TOML parser for these two tokens.
    let pyproject_content = read_file_to_string(dir, "pyproject.toml");
    if let Some(content) = pyproject_content.as_deref() {
        if content.contains("[tool.poetry]") {
            matches.push(Detection {
                runtime: Runtime::PythonPoetry,
                confidence: Confidence::High,
                marker: Some("pyproject.toml [tool.poetry]".to_string()),
            });
        }
        if content.contains("[tool.uv]") || file_exists(dir, "uv.lock") {
            let marker = if file_exists(dir, "uv.lock") {
                "uv.lock"
            } else {
                "pyproject.toml [tool.uv]"
            };
            matches.push(Detection {
                runtime: Runtime::PythonUv,
                confidence: Confidence::High,
                marker: Some(marker.to_string()),
            });
        }
    }

    if file_exists(dir, "Pipfile") {
        matches.push(Detection {
            runtime: Runtime::PythonPipenv,
            confidence: Confidence::High,
            marker: Some("Pipfile".to_string()),
        });
    }

    // Bare requirements.txt qualifies as PythonPip ONLY when
    // no stronger Python marker fired. Operators with full
    // poetry / uv / pipenv setups often keep а requirements.
    // txt for CI side-effects; we don't want to misclassify
    // those as bare-pip.
    let stronger_python_seen = matches.iter().any(|d| {
        matches!(
            d.runtime,
            Runtime::PythonPoetry | Runtime::PythonUv | Runtime::PythonPipenv
        )
    });
    if !stronger_python_seen && file_exists(dir, "requirements.txt") {
        matches.push(Detection {
            runtime: Runtime::PythonPip,
            confidence: Confidence::Medium,
            marker: Some("requirements.txt".to_string()),
        });
    }

    // ── Compiled runtimes ─────────────────────────────────
    if file_exists(dir, "Cargo.toml") {
        matches.push(Detection {
            runtime: Runtime::Rust,
            confidence: Confidence::High,
            marker: Some("Cargo.toml".to_string()),
        });
    }
    if file_exists(dir, "go.mod") {
        matches.push(Detection {
            runtime: Runtime::Go,
            confidence: Confidence::High,
            marker: Some("go.mod".to_string()),
        });
    }

    // ── Docker (Low) ──────────────────────────────────────
    // Dockerfile alone is а weak signal — typical of polyglot
    // projects where the runtime is opaque to apprafter and
    // the operator wants the docker build-only template.
    // Strictly Low so the wizard always asks even when nothing
    // else matched.
    if file_exists(dir, "Dockerfile") && matches.is_empty() {
        matches.push(Detection {
            runtime: Runtime::Docker,
            confidence: Confidence::Low,
            marker: Some("Dockerfile".to_string()),
        });
    }

    // ── Fallback ──────────────────────────────────────────
    if matches.is_empty() {
        matches.push(Detection {
            runtime: Runtime::Blank,
            confidence: Confidence::Fallback,
            marker: None,
        });
    }

    matches
}

fn file_exists(dir: &Path, name: &str) -> bool {
    dir.join(name).is_file()
}

fn read_file_to_string(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Helper — create one or more empty files in а tempdir.
    fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        for (name, content) in files {
            fs::write(dir.path().join(name), content).expect("write fixture");
        }
        dir
    }

    #[test]
    fn slug_is_kebab_case_for_every_variant() {
        // Load-bearing — Part 3's templates are named
        // `<slug>.cue.hbs`. If а variant slugs to something
        // weird, the template lookup breaks silently.
        for r in [
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
        ] {
            let s = r.slug();
            assert!(
                s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "slug `{s}` for {r:?} must be kebab-case [a-z0-9-]"
            );
            assert!(!s.starts_with('-') && !s.ends_with('-'));
        }
    }

    #[test]
    fn detects_bun_via_lockfile() {
        // `bun.lock` (textual lockfile, modern) — High.
        let d = fixture(&[("bun.lock", ""), ("package.json", "{}")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].runtime, Runtime::Bun);
        assert_eq!(r[0].confidence, Confidence::High);
        assert_eq!(r[0].marker.as_deref(), Some("bun.lock"));
    }

    #[test]
    fn detects_bun_via_legacy_lockb() {
        // `bun.lockb` (binary lockfile, legacy) — also High.
        let d = fixture(&[("bun.lockb", ""), ("package.json", "{}")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::Bun);
        assert_eq!(r[0].marker.as_deref(), Some("bun.lockb"));
    }

    #[test]
    fn detects_pnpm_only_when_pnpm_lock_present() {
        let d = fixture(&[("pnpm-lock.yaml", ""), ("package.json", "{}")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::NodePnpm);
        assert_eq!(r[0].confidence, Confidence::High);
    }

    #[test]
    fn detects_yarn_only_when_yarn_lock_present() {
        let d = fixture(&[("yarn.lock", ""), ("package.json", "{}")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::NodeYarn);
    }

    #[test]
    fn detects_npm_when_package_lock_present() {
        let d = fixture(&[("package-lock.json", ""), ("package.json", "{}")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::NodeNpm);
        assert_eq!(r[0].confidence, Confidence::High);
    }

    #[test]
    fn bare_package_json_defaults_to_npm_with_medium_confidence() {
        // No lockfile → can't tell which package manager
        // ships. Default to npm (lowest common denominator)
        // but Medium so the wizard prompts.
        let d = fixture(&[("package.json", "{}")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::NodeNpm);
        assert_eq!(r[0].confidence, Confidence::Medium);
    }

    #[test]
    fn lockfile_priority_beats_bare_package_json() {
        // bun.lock + package.json — bun wins (High), no
        // duplicate node-npm Medium entry. Mirrors the
        // marker-evaluation order in `detect_runtimes`.
        let d = fixture(&[("bun.lock", ""), ("package.json", "{}")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].runtime, Runtime::Bun);
    }

    #[test]
    fn detects_python_poetry_from_pyproject_section() {
        let pyproject = "[tool.poetry]\nname = \"foo\"\n";
        let d = fixture(&[("pyproject.toml", pyproject)]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::PythonPoetry);
        assert_eq!(r[0].confidence, Confidence::High);
        assert!(
            r[0].marker.as_deref().unwrap().contains("[tool.poetry]"),
            "marker must surface the section header so the wizard \
             can show why we picked poetry — got: {:?}",
            r[0].marker
        );
    }

    #[test]
    fn detects_python_uv_from_pyproject_section() {
        let pyproject = "[tool.uv]\nrequires-python = \">=3.12\"\n";
        let d = fixture(&[("pyproject.toml", pyproject)]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::PythonUv);
    }

    #[test]
    fn detects_python_uv_from_lockfile_when_pyproject_lacks_marker() {
        // uv.lock present but pyproject.toml says nothing
        // about uv — still а strong signal (uv only writes
        // its lockfile when it's the manager). Prefer the
        // lockfile-based marker.
        let pyproject = "[project]\nname = \"foo\"\n";
        let d = fixture(&[("pyproject.toml", pyproject), ("uv.lock", "")]);
        let r = detect_runtimes(d.path());
        assert!(r.iter().any(|d| d.runtime == Runtime::PythonUv));
    }

    #[test]
    fn detects_pipenv_from_pipfile() {
        let d = fixture(&[("Pipfile", "")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::PythonPipenv);
    }

    #[test]
    fn bare_requirements_txt_is_pip_only_without_stronger_python_signal() {
        let d = fixture(&[("requirements.txt", "flask\n")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::PythonPip);
        assert_eq!(r[0].confidence, Confidence::Medium);
    }

    #[test]
    fn requirements_txt_skipped_when_poetry_already_matched() {
        // Common CI pattern: poetry-managed project but а
        // `requirements.txt` shipped alongside для tools that
        // don't grok poetry. Detection must NOT add а bogus
        // python-pip alongside python-poetry.
        let pyproject = "[tool.poetry]\nname = \"foo\"\n";
        let d = fixture(&[
            ("pyproject.toml", pyproject),
            ("requirements.txt", "flask\n"),
        ]);
        let r = detect_runtimes(d.path());
        assert_eq!(
            r.iter().filter(|d| d.runtime == Runtime::PythonPip).count(),
            0,
            "python-pip must NOT appear when python-poetry is detected; got: {r:?}"
        );
    }

    #[test]
    fn detects_rust_from_cargo_toml() {
        let d = fixture(&[("Cargo.toml", "[package]\nname = \"x\"\n")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::Rust);
        assert_eq!(r[0].confidence, Confidence::High);
    }

    #[test]
    fn detects_go_from_go_mod() {
        let d = fixture(&[("go.mod", "module x\n")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::Go);
    }

    #[test]
    fn detects_docker_only_as_last_resort_when_alone() {
        // Bare Dockerfile с nothing else — Low confidence.
        let d = fixture(&[("Dockerfile", "FROM alpine\n")]);
        let r = detect_runtimes(d.path());
        assert_eq!(r[0].runtime, Runtime::Docker);
        assert_eq!(r[0].confidence, Confidence::Low);
    }

    #[test]
    fn dockerfile_alongside_lockfile_does_not_add_docker_entry() {
        // Operator с bun + Dockerfile (containerised bun app)
        // gets bun detection only; Dockerfile is implementation
        // detail, not the runtime signal here.
        let d = fixture(&[
            ("bun.lock", ""),
            ("package.json", "{}"),
            ("Dockerfile", "FROM oven/bun\n"),
        ]);
        let r = detect_runtimes(d.path());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].runtime, Runtime::Bun);
    }

    #[test]
    fn empty_dir_falls_back_to_blank() {
        let d = tempdir().unwrap();
        let r = detect_runtimes(d.path());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].runtime, Runtime::Blank);
        assert_eq!(r[0].confidence, Confidence::Fallback);
        assert!(r[0].marker.is_none());
    }

    #[test]
    fn multi_stack_monorepo_surfaces_all_matches() {
        // Operator runs `apprafter app add` from а dir
        // carrying both bun.lock (frontend) и Cargo.toml
        // (backend) — both Highs surface; caller's prompt
        // shows the list, defaults к first alphabetically.
        let d = fixture(&[
            ("bun.lock", ""),
            ("package.json", "{}"),
            ("Cargo.toml", "[package]\n"),
        ]);
        let r = detect_runtimes(d.path());
        let runtimes: Vec<Runtime> = r.iter().map(|d| d.runtime).collect();
        assert!(
            runtimes.contains(&Runtime::Bun) && runtimes.contains(&Runtime::Rust),
            "multi-stack must surface both; got: {runtimes:?}"
        );
        assert!(r.iter().all(|d| d.confidence == Confidence::High));
    }

    #[test]
    fn ignores_directories_named_like_markers() {
        // Edge case: а directory called `Dockerfile/` или
        // `package.json/` (rare but possible — generated
        // build output). `file_exists` uses `is_file()` so
        // these are skipped. Defensive guard so we don't
        // misclassify а build-artefact dir as а runtime.
        let d = tempdir().unwrap();
        fs::create_dir(d.path().join("Dockerfile")).unwrap();
        fs::create_dir(d.path().join("package.json")).unwrap();
        let r = detect_runtimes(d.path());
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].runtime, Runtime::Blank);
    }
}
