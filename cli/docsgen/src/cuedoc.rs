// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Vet the complete CUE manifests printed in the documentation against
//! the schema the CLI ships with, so a manifest a reader can copy is a
//! manifest that still validates.
//!
//! # Complete documents only
//!
//! A fence qualifies when it is a CUE **file**: a `package` clause
//! first, and the AppRafter schema imported. Both halves are load-
//! bearing, and [`is_complete_document`] is where the rule lives.
//!
//! Fragments — `spec: base: expose: { … }` and its six sibling depths —
//! are deliberately out. One skeleton cannot wrap seven anchor depths,
//! and every fence would need a hand-authored anchor the gate cannot
//! verify. The obvious shortcut is worse than nothing: wrapping a
//! fragment in a `#`-definition makes `#x: v1alpha1.#TotallyBogusKind &
//! {}` vet **green**, so the check would pass everything it was built to
//! catch. Only a concrete wrapper errors correctly, and that in turn
//! forces `metadata.name` and every other concrete-required field onto a
//! snippet whose author never wrote them.
//!
//! # One batch, not one process per snippet
//!
//! `cue` costs 69 ms per invocation inside `nix develop` and 599 ms
//! outside it, where the repo's `~/bin/cue` shim spawns `nix run` per
//! subprocess. A pre-commit hook pays whichever the contributor has, so
//! the whole corpus goes through **one** `cue vet`: a single module
//! root, the schema bundle laid once, one directory per document (each
//! its own package — a flat directory would collide the copies into one)
//! and `cue vet -c -i ./...` over the lot.
//!
//! `-i` is not optional. Without it cue stops at the first failing
//! package and hides every other, which costs one CI round-trip per bad
//! fence; the test that pins this is
//! `one_bad_document_does_not_hide_the_others`.
//!
//! # Which `cue` this runs
//!
//! The binary is whatever `CUE_BIN` names, else `cue` on `PATH` — the
//! same resolution `apprafter app validate` uses. Under `nix develop`
//! that is currently CUE **v0.16.1**, while twenty CI steps pin
//! **v0.10.0** through `setup-cue`: this gate therefore evaluates on a
//! different `cue` than most of CI does. The injected
//! `cue.mod/module.cue` pins `language.version: v0.10.0`, so the
//! *language semantics* are the pinned ones either way; the divergence
//! that remains is the evaluator's own behaviour and its diagnostics.
//! Recorded here as a known fact, not fixed here.
//!
//! # What this check does not reproduce
//!
//! A manifest using the bare `claim.<type>.<field>` env references of
//! ADR 0046 resolves against a binding the cue-cmp generates at render
//! time and nobody commits. `apprafter app validate` reproduces that
//! two-pass generation; this module does not, so such a document would
//! fail here for a reason that is not drift. No documented manifest uses
//! one today. If one appears, extend this module rather than deleting
//! the finding.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;
use std::process::Command;

use apprafter::docs_api::{WORKSPACE_MODULE_CUE, WORKSPACE_SCHEMAS};

/// One complete CUE document lifted out of the documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Where it came from, as the reader would grep for it —
    /// `docs/dev-guide/application-cue.md:42`, the line of the fence's
    /// opening delimiter. Carried through untouched so a finding names
    /// a place rather than a temp directory.
    pub origin: String,
    /// The fence body, verbatim. Nothing is prepended: the document is
    /// written to disk exactly as the author wrote it, so cue's line
    /// numbers are offsets into the fence and need no correction beyond
    /// adding the fence's own line.
    pub body: String,
}

/// One thing `cue vet` objected to, attributed to the document it came
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The [`Document::origin`] of the document at fault.
    pub origin: String,
    /// cue's diagnostic, with the temp-workspace path removed — the
    /// path is an implementation detail and naming it in a report sends
    /// the reader to a directory that no longer exists.
    pub message: String,
    /// 1-based line **within the fence body**, when cue reported one.
    /// The absolute source line is the fence's line plus this, because
    /// the document is laid out unwrapped and unshifted — nothing is
    /// prepended, so no offset correction is needed and none can be got
    /// wrong.
    pub line: Option<usize>,
    /// 1-based column within that line.
    ///
    /// Exact for a fence at column 0, which is every documented manifest
    /// today. `scan` strips a fence's own indentation and blockquote
    /// depth from its body, so for a fence indented under a list item or
    /// quoted in a callout the column is relative to the **stripped**
    /// body and the reader's editor will show it that many characters
    /// further right. The line is unaffected either way, which is the
    /// number that finds the place.
    pub column: Option<usize>,
}

impl std::fmt::Display for Finding {
    /// `docs/x.md:42 (+5:63): app.spec…: field not allowed` — the
    /// `(+line:col)` is relative to the fence body, and says so.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(column)) => {
                write!(f, "{} (+{line}:{column}): {}", self.origin, self.message)
            }
            (Some(line), None) => write!(f, "{} (+{line}): {}", self.origin, self.message),
            _ => write!(f, "{}: {}", self.origin, self.message),
        }
    }
}

/// The filename every document is written as, inside its own directory.
/// Both halves are matched verbatim when attributing a diagnostic, so
/// they must not look like anything a document could contain.
const SNIPPET: &str = "snippet.cue";

/// The substring that marks a location in cue's output as ours.
const LOCATION_MARKER: &str = "/snippet.cue:";

/// Whether a fence body is a complete CUE document this check can vet.
///
/// Two conditions, and the pair is the whole rule:
///
/// 1. **a `package` clause first**, ignoring blank lines and `//`
///    comments. This is CUE's own definition of a file, and it is what
///    separates a document from the fragments that anchor mid-struct.
///    It is also a correctness requirement rather than a nicety: `cue
///    vet ./...` **silently skips** a directory whose file has no
///    package clause, so a fragment admitted here would not fail — it
///    would pass without being looked at, which is the one outcome a
///    gate must never have.
/// 2. **the AppRafter schema imported.** A CUE file that does not
///    reference `apprafter.io/schemas/v1alpha1` unifies against nothing
///    and vets green whatever it says, so validating it proves nothing
///    about the product. This is also what keeps the rule off other
///    languages: `package main` with Go imports fails it.
///
/// Detected from content, never from the fence's language tag — the
/// corpus tags shell as `sh`, `bash`, `console` and nothing at all, and
/// a tag-keyed rule sees a fraction of any surface. The four `sh` fences
/// that carry heredoc'd or `kubectl`-emitted YAML fail condition 1 on
/// their first line (`cat >…`, `kubectl -n demo …`), which is the point:
/// a rule that tried to vet `kubectl -n demo delete …` as a manifest
/// would be wrong about what it was reading.
pub fn is_complete_document(body: &str) -> bool {
    package_clause(body).is_some() && body.contains("\"apprafter.io/schemas/v1alpha1\"")
}

/// The declared package name, if the file opens with a package clause.
///
/// Leading blank lines and `//` comments are skipped — the documented
/// manifests open with `// apprafter/Application.cue`, and every file in
/// this repository opens with an SPDX header.
fn package_clause(body: &str) -> Option<&str> {
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let rest = line.strip_prefix("package ")?;
        let name: &str = rest
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .split("//")
            .next()
            .unwrap_or_default();
        return (!name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '#'))
        .then_some(name);
    }
    None
}

/// Vet every document in one batch, returning what cue objected to.
///
/// `Ok(vec![])` means every document validated. `Err` is reserved for a
/// failure of the *harness* — no `cue` on `PATH`, an unwritable temp
/// directory, or a diagnostic that cannot be attributed to any document
/// — because those say something is wrong with this check, not with the
/// documentation, and the two must not be reported as the same thing.
///
/// The temp workspace is a [`tempfile::TempDir`] held for the whole
/// call, so it is removed on the error paths and on unwind exactly as it
/// is on success.
pub fn validate_documents(documents: &[Document]) -> Result<Vec<Finding>, Box<dyn Error>> {
    if documents.is_empty() {
        return Ok(Vec::new());
    }

    let workdir = tempfile::tempdir()?;
    // `cue`'s `./...` skips any directory whose name starts with `.`,
    // and `tempfile` names its directory `.tmpXXXX`. Running from a
    // plain subdirectory is what stops the batch from matching no
    // packages and passing vacuously.
    let root = workdir.path().join("workspace");
    std::fs::create_dir_all(&root)?;
    lay_out_schema(&root)?;

    let mut findings = Vec::new();
    // Directory name → origin. `BTreeMap` so the unattributed-diagnostic
    // error message lists directories in a stable order.
    let mut origins: BTreeMap<String, &str> = BTreeMap::new();
    for (index, document) in documents.iter().enumerate() {
        let directory = format!("doc{index:04}");
        if package_clause(&document.body).is_none() {
            // cue would skip this file without a word. Refusing it here
            // is the difference between a gap and a lie.
            findings.push(Finding {
                origin: document.origin.clone(),
                message: "not a CUE document: no `package` clause, which `cue vet` \
                          would skip silently rather than check"
                    .to_string(),
                line: None,
                column: None,
            });
            continue;
        }
        let dir = root.join(&directory);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(SNIPPET), &document.body)?;
        origins.insert(directory, document.origin.as_str());
    }
    if origins.is_empty() {
        return Ok(findings);
    }

    let output = match Command::new(cue_bin())
        .current_dir(&root)
        .args(["vet", "-c", "-i", "./..."])
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Same actionable remedy as `cli_core::CliError::CueNotFound`
            // spells out for `apprafter app validate`; spelled out here
            // rather than borrowed, because that variant carries the
            // remedy in a miette `help` attribute that `Display` drops.
            return Err(
                "`cue` binary not found on PATH — run `nix develop` from the repo \
                        root, which puts `cue` on PATH, or see docs/contributing/setup.md \
                        for direct-install options."
                    .into(),
            );
        }
        Err(err) => return Err(format!("running cue vet: {err}").into()),
    };
    if output.status.success() {
        return Ok(findings);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    findings.extend(attribute(&stderr, &origins)?);
    Ok(findings)
}

/// Write `cue.mod/module.cue` and the schema bundle the documents
/// import.
///
/// Both come from `apprafter`'s `docs_api`, which is the same bundle
/// `apprafter app validate` lays: a reader who runs that command on a
/// manifest copied out of the docs must get this gate's verdict, and
/// they cannot if the two embed different copies.
fn lay_out_schema(root: &Path) -> Result<(), Box<dyn Error>> {
    let module = root.join("cue.mod");
    let pkg = module.join("pkg/apprafter.io/schemas/v1alpha1");
    std::fs::create_dir_all(&pkg)?;
    std::fs::write(module.join("module.cue"), WORKSPACE_MODULE_CUE)?;
    for (name, content) in WORKSPACE_SCHEMAS {
        std::fs::write(pkg.join(name), content)?;
    }
    Ok(())
}

/// Turn cue's stderr into findings against the documents that caused
/// them.
///
/// cue prints a diagnostic as an unindented message line optionally
/// followed by indented location lines, and sometimes with the location
/// appended to the message line itself. Both shapes occur, so a record
/// is "an unindented line plus everything indented under it" and the
/// location is searched for across the whole record.
///
/// A record naming no document is an `Err`: it means the workspace is
/// wrong — a schema laid badly, a cue version that dislikes the module
/// file — and reporting our own breakage as documentation drift would
/// send the reader to fix a page that is fine.
fn attribute(
    stderr: &str,
    origins: &BTreeMap<String, &str>,
) -> Result<Vec<Finding>, Box<dyn Error>> {
    let mut findings = Vec::new();
    for record in records(stderr) {
        let joined = record.join("\n");
        let Some((directory, line, column)) = locate(&joined) else {
            return Err(format!(
                "cue vet reported a problem that names none of the {} document(s) \
                 under test, so it is this check that is broken, not the docs:\n{}",
                origins.len(),
                joined
            )
            .into());
        };
        let Some(origin) = origins.get(&directory) else {
            return Err(format!(
                "cue vet named `{directory}`, which is not one of the directories \
                 this batch laid out:\n{joined}"
            )
            .into());
        };
        findings.push(Finding {
            origin: (*origin).to_string(),
            message: strip_location(&record[0]).to_string(),
            line: Some(line),
            column: Some(column),
        });
    }
    Ok(findings)
}

/// Group stderr into records: an unindented line and its indented
/// continuation lines.
fn records(stderr: &str) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    for line in stderr.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            match out.last_mut() {
                Some(record) => record.push(line.trim().to_string()),
                // An indented line with no message above it: keep it as
                // its own record rather than dropping it, so nothing cue
                // said disappears.
                None => out.push(vec![line.trim().to_string()]),
            }
        } else {
            out.push(vec![line.to_string()]);
        }
    }
    out
}

/// The first `…/<directory>/snippet.cue:<line>:<column>` in a record.
fn locate(record: &str) -> Option<(String, usize, usize)> {
    let at = record.find(LOCATION_MARKER)?;
    let directory = record[..at]
        .rsplit(['/', ' ', '\t'])
        .next()
        .unwrap_or_default()
        .to_string();
    let rest = &record[at + LOCATION_MARKER.len()..];
    let mut numbers = rest.split(':');
    let line = take_number(numbers.next()?)?;
    let column = take_number(numbers.next()?)?;
    (!directory.is_empty()).then_some((directory, line, column))
}

/// The leading run of digits, as a number.
fn take_number(text: &str) -> Option<usize> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// A message line without the temp-workspace location cue appended to
/// it, and without its trailing `:` separator.
fn strip_location(line: &str) -> &str {
    let cut = match line.find(LOCATION_MARKER) {
        // Back up to the whitespace before the path token, so the
        // message keeps everything the author needs and loses only the
        // path they cannot use.
        Some(at) => line[..at].rfind([' ', '\t']).map(|s| s + 1).unwrap_or(0),
        None => line.len(),
    };
    line[..cut].trim_end().trim_end_matches(':').trim_end()
}

/// `cue` binary path — `CUE_BIN`, else `cue` on `PATH`, exactly as
/// `apprafter app validate` resolves it, so both reach the same one.
fn cue_bin() -> String {
    std::env::var("CUE_BIN").unwrap_or_else(|_| "cue".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_clause_is_found_past_comments_and_blank_lines() {
        assert_eq!(package_clause("package apprafter\n"), Some("apprafter"));
        assert_eq!(
            package_clause("// apprafter/Application.cue\n\npackage apprafter\n"),
            Some("apprafter")
        );
        assert_eq!(package_clause("package example // why\n"), Some("example"));
    }

    #[test]
    fn a_fragment_has_no_package_clause() {
        assert_eq!(
            package_clause("spec: base: expose: {\n\tport: 8080\n}\n"),
            None
        );
        assert_eq!(package_clause("needs: {\n\tpg: {}\n}\n"), None);
        assert_eq!(package_clause(""), None);
        // A shell fence: the first line settles it.
        assert_eq!(package_clause("kubectl -n demo delete pod x\n"), None);
        assert_eq!(
            package_clause("cat > apprafter/Application.cue <<'EOF'\n"),
            None
        );
    }

    #[test]
    fn a_complete_document_needs_both_a_package_and_the_schema_import() {
        let document = "package apprafter\n\nimport v1alpha1 \"apprafter.io/schemas/v1alpha1\"\n\napp: v1alpha1.#Application & {}\n";
        assert!(is_complete_document(document));
        // A package clause alone unifies against nothing and would vet
        // green whatever it said.
        assert!(!is_complete_document("package apprafter\n\nx: 1\n"));
        // The import alone, mid-struct, is a fragment.
        assert!(!is_complete_document(
            "spec: base: image: \"apprafter.io/schemas/v1alpha1\"\n"
        ));
        // Go, which also has a leading package clause.
        assert!(!is_complete_document("package main\n\nimport \"fmt\"\n"));
    }

    #[test]
    fn a_heredocd_manifest_is_a_shell_fence_not_a_document() {
        // The manifest is complete, but the fence is a shell command:
        // its first line is `cat >`, so the rule reads it as shell —
        // which is what it is.
        let fence = "cat > apprafter/Application.cue <<'EOF'\npackage apprafter\nimport v1alpha1 \"apprafter.io/schemas/v1alpha1\"\nEOF\n";
        assert!(!is_complete_document(fence));
    }

    #[test]
    fn a_record_is_a_message_plus_its_indented_locations() {
        let stderr = "first: field not allowed:\n    ./doc0000/snippet.cue:5:63\nsecond: bad:\n    ./doc0001/snippet.cue:1:1\n";
        let parsed = records(stderr);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].len(), 2);
        assert_eq!(parsed[1][1], "./doc0001/snippet.cue:1:1");
    }

    #[test]
    fn a_location_names_its_directory_line_and_column() {
        assert_eq!(
            locate("x: bad:\n./doc0007/snippet.cue:12:34"),
            Some(("doc0007".to_string(), 12, 34))
        );
        // The single-line form, which cue also emits.
        assert_eq!(
            locate("x: undefined field: y: ./doc0000/snippet.cue:5:67"),
            Some(("doc0000".to_string(), 5, 67))
        );
        assert_eq!(locate("some instances are incomplete"), None);
    }

    #[test]
    fn the_message_keeps_its_text_and_loses_the_temp_path() {
        assert_eq!(
            strip_location("app.spec.base.expose.public: field not allowed:"),
            "app.spec.base.expose.public: field not allowed"
        );
        assert_eq!(
            strip_location("app.env.X: undefined field: redis: ./doc0000/snippet.cue:5:67"),
            "app.env.X: undefined field: redis"
        );
    }

    #[test]
    fn an_unattributable_diagnostic_is_our_bug_not_the_docs() {
        let origins = BTreeMap::from([("doc0000".to_string(), "a.md:1")]);
        let err = attribute("some instances are incomplete\n", &origins)
            .expect_err("a diagnostic naming no document must not become a doc finding");
        assert!(
            err.to_string().contains("this check that is broken"),
            "{err}"
        );
    }

    #[test]
    fn a_finding_renders_as_a_place_a_reader_can_open() {
        let finding = Finding {
            origin: "docs/dev-guide/application-cue.md:42".to_string(),
            message: "app.spec.base.expose.public: field not allowed".to_string(),
            line: Some(15),
            column: Some(20),
        };
        assert_eq!(
            finding.to_string(),
            "docs/dev-guide/application-cue.md:42 (+15:20): \
             app.spec.base.expose.public: field not allowed"
        );
    }

    #[test]
    fn an_empty_batch_runs_no_cue_at_all() {
        assert_eq!(validate_documents(&[]).unwrap(), Vec::new());
    }
}
