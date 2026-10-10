// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Every pointer to where a Hetzner Cloud API token is created says it one way:
//! `cli_core::target::HETZNER_API_TOKENS_PAGE`, the console's current name and its own labels
//! (WI-454).
//!
//! The pointers had drifted into five spellings across the CLI's helps, the doctor hint, the
//! `--token` help, the core's client-neutral help, the desktop and the operator guide, under the
//! console's old name, and one told the reader to copy a token out of a console that shows it
//! only once. Code that can use the constant does; text that cannot (a doc comment that clap
//! turns into help, a Markdown page, a golden, a TypeScript test) is held to it here.
//!
//! The scan reads the tracked prose and code of the CLI, the desktop and the docs, joins wrapped
//! lines (a Rust string's `\` continuation, comment markers, miette's and Markdown's line breaks)
//! and finds every console path that names the token page: `Security` or `Access`, a separator,
//! then `API token(s)`. Each must be the whole constant. A short list of old wordings that name
//! no path is refused outright. Historical records (ADRs, the changelog archive) are not scanned.

use std::fs;
use std::path::{Path, PathBuf};

use cli_core::target::HETZNER_API_TOKENS_PAGE;

/// Directories scanned, relative to the repository root, with the file extensions read in each.
const SCANNED: &[(&str, &[&str])] = &[
    ("cli", &["rs", "golden"]),
    ("docs", &["md", "json"]),
    ("desktop/src", &["ts", "tsx", "json"]),
    ("desktop/src-tauri/src", &["rs"]),
    ("desktop/ipc", &["rs"]),
];

/// Single files scanned besides [`SCANNED`].
const SCANNED_FILES: &[&str] = &["README.md"];

/// Never scanned: build output, dependencies, and records of what was true at the time.
const SKIPPED: &[&str] = &[
    // This file spells the old pointers out, as the cases its matcher must find.
    "cli/platform-cli/tests/hetzner_token_page_test.rs",
    "docs/adr",
    "docs/changelog",
    "docs/superpowers",
    "desktop/design-source",
];

/// One file of each kind the scan must read, each of which carries the pointer: a definition
/// in source, a golden in the directory named `target`, an operator-guide page, the generated
/// CLI reference and the desktop's generated constants. A scan that stopped reading one of them
/// would pass on whatever it no longer sees.
const MUST_CARRY: &[&str] = &[
    "cli/cli-core/src/target.rs",
    "cli/platform-cli/tests/golden/target/renew_no_token.golden",
    "docs/operator-guide/troubleshooting.md",
    "docs/reference/cli/target.md",
    "desktop/src/ipc/generated/target.ts",
];

/// Old wordings that name no path, refused wherever they appear (compared case-insensitively
/// on the joined text).
const OLD_WORDINGS: &[&str] = &[
    "cloud console shows a token",
    "create one in the hetzner cloud console",
    "copy it whole out of",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

fn collect(root: &Path, dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if SKIPPED.iter().any(|s| rel_str == *s) {
            continue;
        }
        if path.is_dir() {
            // A cargo target dir is marked by its CACHEDIR.TAG, not by its name: the goldens of
            // the `target` commands live in a directory called `target`.
            let name = entry.file_name();
            if path.join("CACHEDIR.TAG").exists()
                || name == "node_modules"
                || name.to_string_lossy().starts_with('.')
            {
                continue;
            }
            collect(root, &path, exts, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e))
        {
            out.push(path);
        }
    }
}

/// The file as prose: wrapped lines joined, comment and box-drawing prefixes dropped, every run
/// of whitespace one space.
fn prose(text: &str) -> String {
    let mut joined = String::with_capacity(text.len());
    let mut continued = false;
    for line in text.lines() {
        let mut l = line.trim_start();
        for marker in ["///", "//!", "//", "│", "*"] {
            if let Some(rest) = l.strip_prefix(marker) {
                l = rest.trim_start();
                break;
            }
        }
        if !continued {
            joined.push(' ');
        }
        // A Rust string's `\` at the end of a line drops the newline and the next line's
        // indentation: the text runs on.
        continued = l.ends_with('\\') && !l.ends_with("\\\\");
        joined.push_str(if continued { &l[..l.len() - 1] } else { l });
    }
    joined.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Byte offsets in `text` where a console path to the token page starts (`Security` or `Access`,
/// a separator, `API token`), with the decoration Markdown and quotes put around the labels.
fn token_paths(text: &str) -> Vec<usize> {
    const DECORATION: &[char] = &[' ', '*', '_', '`', '"', '\'', '“', '”'];
    const SEPARATORS: &[char] = &['→', '›', '»', '>', '/', '-', ','];
    let mut found = Vec::new();
    for lead in ["Security", "Access"] {
        for (at, _) in text.match_indices(lead) {
            let rest = text[at + lead.len()..].trim_start_matches(DECORATION);
            let Some(sep) = rest.chars().next().filter(|c| SEPARATORS.contains(c)) else {
                continue;
            };
            let label = rest[sep.len_utf8()..].trim_start_matches(DECORATION);
            if label
                .get(..9)
                .is_some_and(|l| l.eq_ignore_ascii_case("API token"))
            {
                found.push(at);
            }
        }
    }
    found
}

#[test]
fn every_token_page_pointer_is_the_shared_wording() {
    let root = repo_root();
    let mut files = Vec::new();
    for (dir, exts) in SCANNED {
        collect(&root, &root.join(dir), exts, &mut files);
    }
    files.extend(SCANNED_FILES.iter().map(|f| root.join(f)));
    assert!(
        files.len() > 500,
        "the scan found only {} files under {root:?}: a guard that read nothing is no guard",
        files.len()
    );

    let lead = HETZNER_API_TOKENS_PAGE
        .find("Security")
        .expect("the constant names the Security page");
    let mut carriers = Vec::new();
    let mut problems = Vec::new();
    for file in &files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        let text = prose(&text);
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        for at in token_paths(&text) {
            let is_canonical = at
                .checked_sub(lead)
                .and_then(|start| text.get(start..start + HETZNER_API_TOKENS_PAGE.len()))
                == Some(HETZNER_API_TOKENS_PAGE);
            if is_canonical {
                carriers.push(rel.clone());
            } else {
                let mut from = at.saturating_sub(70);
                while !text.is_char_boundary(from) {
                    from -= 1;
                }
                let mut to = (at + 40).min(text.len());
                while !text.is_char_boundary(to) {
                    to += 1;
                }
                problems.push(format!("{rel}: …{}…", &text[from..to]));
            }
        }
        let lower = text.to_lowercase();
        for old in OLD_WORDINGS {
            if lower.contains(old) {
                problems.push(format!("{rel}: an old wording, {old:?}"));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "say where a token is created with cli_core::target::HETZNER_API_TOKENS_PAGE, \
         {HETZNER_API_TOKENS_PAGE:?}, word for word:\n{}",
        problems.join("\n")
    );
    for file in MUST_CARRY {
        assert!(
            carriers.iter().any(|c| c == file),
            "{file} carries the pointer, but the scan did not find it there: it no longer reads \
             that kind of file"
        );
    }
}

#[test]
fn prose_joins_what_the_scanned_formats_wrap() {
    // A Rust string continued with `\`.
    assert_eq!(
        prose("\"open the \\\n     project\""),
        "\"open the project\""
    );
    // A doc comment and a Markdown paragraph wrapped mid-phrase.
    assert_eq!(
        prose("/// open the project, then\n    /// Security → API tokens"),
        "open the project, then Security → API tokens"
    );
    // A miette help wrapped inside a nested diagnostic's box.
    assert_eq!(
        prose("help: then Security →\n        │ API tokens"),
        "help: then Security → API tokens"
    );
}

#[test]
fn token_paths_finds_every_spelling_the_pointers_had() {
    for spelling in [
        "Cloud Console → Security → API Tokens.",
        "(Security › API tokens)",
        "under Security → API tokens (AppRafter",
        "**Security** → **API tokens**",
        "Access > API Tokens",
        "Security / API tokens",
    ] {
        assert_eq!(token_paths(spelling).len(), 1, "{spelling}");
    }
    for unrelated in [
        "Security in the left menu",
        "Security → Firewalls",
        "an API token",
    ] {
        assert!(token_paths(unrelated).is_empty(), "{unrelated}");
    }
}
