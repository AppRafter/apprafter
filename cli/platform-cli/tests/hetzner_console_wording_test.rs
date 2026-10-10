// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Hetzner console is named one way everywhere in the repository (WI-454): the Hetzner
//! Console, at console.hetzner.com, the name and address docs.hetzner.com uses for it now.
//!
//! Two checks over every tracked file (`git ls-files`), with the wrapped lines of each joined
//! first ([`prose`]):
//!
//! 1. **The token page.** Every console path that names the page where an API token is created
//!    (`Security` or `Access`, a separator, then `API token(s)`) is the whole
//!    `cli_core::target::HETZNER_API_TOKENS_PAGE`, and three old wordings that name no path are
//!    refused. The pointers had drifted into five spellings across the CLI's helps, the doctor
//!    hint, the `--token` help, the core's client-neutral help, the desktop and the operator
//!    guide, and one told the reader to copy a token out of a console that shows it only once.
//! 2. **The old name and address.** Neither "Hetzner Cloud Console" (or a bare "Cloud Console")
//!    nor a `console.hetzner.cloud` URL appears. Another vendor's Cloud Console (Google's, for
//!    one) is not this console and passes ([`OTHER_VENDORS`]). `api.hetzner.cloud` and
//!    `docs.hetzner.cloud` are the API and its reference, which kept their addresses.
//!
//! Only [`HISTORY`] is exempt, each entry with its reason, plus the temporary
//! [`PENDING_REMOVE_WARNING`], which fails the test once nothing is left for it to cover.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cli_core::target::HETZNER_API_TOKENS_PAGE;

/// Tracked paths (or path prefixes ending in `/`) neither check reads, each with why it must
/// keep the words it has.
const HISTORY: &[(&str, &str)] = &[
    (
        "docs/changelog/",
        "the frozen pre-ATM changelog archive: it records each change as it was written then",
    ),
    (
        "plan.md",
        "the roadmap's history: a plan row is never rewritten to match what shipped",
    ),
    (
        "docs/adr/",
        "decision records: each describes the world on the day it was ratified, and later \
         decisions supersede it instead of editing it (docs/adr/README.md)",
    ),
    (
        "desktop/design-source/",
        "a snapshot of the upstream Claude Design project the desktop slices cite \
         (desktop/design-source/README.md), kept as exported",
    ),
    (
        "cli/platform-cli/tests/hetzner_console_wording_test.rs",
        "this file: its matchers' own cases spell the old wordings out",
    ),
];

/// TEMPORARY. `target remove`'s warning, its tests, its goldens and its guide page say "delete
/// server <id> in the Hetzner Cloud Console" (or "delete it in …"). WI-458 is rewriting that
/// warning on `feat/desktop-d3`, so those sites are aligned after WI-458 lands, not here. An
/// old-name hit in one of these files that follows one of those two phrases passes. Once none is
/// left the test fails until this list is deleted, so the exemption cannot outlive its sites.
const PENDING_REMOVE_WARNING: &[&str] = &[
    "cli/platform-cli/src/commands/target.rs",
    "cli/platform-cli/tests/golden/target/remove_",
    "docs/operator-guide/target-store.md",
];

/// Vendors whose own product is called a Cloud Console: "Google Cloud Console" is not the old
/// name of Hetzner's.
const OTHER_VENDORS: &[&str] = &["google", "oracle", "alibaba", "ibm", "huawei", "tencent"];

/// Old wordings that name no path, refused wherever they appear (compared case-insensitively
/// on the joined text).
const OLD_TOKEN_WORDINGS: &[&str] = &[
    "cloud console shows a token",
    "create one in the hetzner cloud console",
    "copy it whole out of",
];

/// One file of each kind the scan must read, each of which carries the token-page pointer: a
/// definition in source, a golden in the directory named `target`, an operator-guide page, the
/// generated CLI reference and the desktop's generated constants. A scan that stopped reading one
/// of them would pass on whatever it no longer sees.
const MUST_CARRY: &[&str] = &[
    "cli/cli-core/src/target.rs",
    "cli/platform-cli/tests/golden/target/renew_no_token.golden",
    "docs/operator-guide/troubleshooting.md",
    "docs/reference/cli/target.md",
    "desktop/src/ipc/generated/target.ts",
];

fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs: this test reads the repository's index");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// The repository root, as git names it (never a `\\?\` verbatim path, which git for Windows
/// does not take back).
fn repo_root() -> PathBuf {
    let top = git(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &["rev-parse", "--show-toplevel"],
    );
    PathBuf::from(String::from_utf8(top).expect("a UTF-8 path").trim_end())
}

/// Every tracked file, as `/`-separated paths relative to the root. The index, not a directory
/// walk: build output, dependencies and local scratch (a gitignored plan, a target dir) are never
/// read, and nothing tracked is skipped for its directory's name.
fn tracked(root: &Path) -> Vec<String> {
    String::from_utf8(git(root, &["ls-files", "-z"]))
        .expect("tracked paths are UTF-8")
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

fn exempt(path: &str) -> bool {
    HISTORY.iter().any(|(p, _)| path.starts_with(p))
}

/// The file as prose: wrapped lines joined, comment and box-drawing prefixes dropped, string
/// literals split with `+` joined back, every run of whitespace one space.
fn prose(text: &str) -> String {
    let mut joined = String::with_capacity(text.len());
    let mut continued = false;
    for line in text.lines() {
        let mut l = line.trim_start();
        for marker in ["///", "//!", "//", "│", "*", "#"] {
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
    let spaced = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    // `'… the Hetzner ' + 'Console …'` (TypeScript, JavaScript, Python): one string.
    let quote = |c: Option<char>| matches!(c, Some('\'' | '"' | '`'));
    let mut out = String::with_capacity(spaced.len());
    let mut rest = spaced.as_str();
    while let Some(i) = rest.find(" + ") {
        let before = rest[..i].chars().last();
        let after = rest[i + 3..].chars().next();
        if quote(before) && before == after {
            out.push_str(&rest[..i - 1]);
            rest = &rest[i + 4..];
        } else {
            out.push_str(&rest[..i + 3]);
            rest = &rest[i + 3..];
        }
    }
    out.push_str(rest);
    out
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

/// Byte offsets in `text` of the console's old name or address: `console.hetzner.cloud`, and
/// `Cloud Console` unless another vendor's name comes right before it.
fn old_console_names(text: &str) -> Vec<usize> {
    let lower = text.to_ascii_lowercase();
    let mut found: Vec<usize> = lower
        .match_indices("console.hetzner.cloud")
        .map(|(at, _)| at)
        .collect();
    for (at, _) in lower.match_indices("cloud console") {
        let vendor = lower[..at]
            .trim_end()
            .rsplit(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or("");
        if !OTHER_VENDORS.contains(&vendor) {
            found.push(at);
        }
    }
    found.sort_unstable();
    found
}

/// Up to `before` bytes before `at` and `after` bytes from it, cut at character boundaries.
fn around(text: &str, at: usize, before: usize, after: usize) -> &str {
    let mut from = at.saturating_sub(before);
    while !text.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (at + after).min(text.len());
    while !text.is_char_boundary(to) {
        to += 1;
    }
    &text[from..to]
}

/// An old-name hit that [`PENDING_REMOVE_WARNING`] covers: in one of its files, right after
/// "delete server <id> in the Hetzner" or "delete it in the Hetzner".
fn pending(path: &str, text: &str, at: usize) -> bool {
    let before = around(text, at, 40, 0);
    PENDING_REMOVE_WARNING.iter().any(|p| path.starts_with(p))
        && before.ends_with("in the Hetzner ")
        && (before.contains("delete server ") || before.contains("delete it in the"))
}

#[test]
fn the_hetzner_console_is_named_one_way_everywhere() {
    let root = repo_root();
    let files = tracked(&root);
    assert!(
        files.len() > 1000,
        "git ls-files listed only {} files under {root:?}: a guard that read nothing is no guard",
        files.len()
    );

    let lead = HETZNER_API_TOKENS_PAGE
        .find("Security")
        .expect("the constant names the Security page");
    let mut carriers = Vec::new();
    let mut pending_hits = 0;
    let mut problems = Vec::new();
    for rel in files.iter().filter(|f| !exempt(f)) {
        // Not UTF-8 (an image, a font): no prose to check.
        let Ok(raw) = fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let text = prose(&raw);
        for at in token_paths(&text) {
            let is_canonical = at
                .checked_sub(lead)
                .and_then(|start| text.get(start..start + HETZNER_API_TOKENS_PAGE.len()))
                == Some(HETZNER_API_TOKENS_PAGE);
            if is_canonical {
                carriers.push(rel.clone());
            } else {
                problems.push(format!(
                    "{rel}: a token page that is not HETZNER_API_TOKENS_PAGE: …{}…",
                    around(&text, at, 70, 40)
                ));
            }
        }
        let lower = text.to_lowercase();
        for old in OLD_TOKEN_WORDINGS {
            if lower.contains(old) {
                problems.push(format!("{rel}: an old token wording, {old:?}"));
            }
        }
        for at in old_console_names(&text) {
            if pending(rel, &text, at) {
                pending_hits += 1;
            } else {
                problems.push(format!(
                    "{rel}: the console's old name or address: …{}…",
                    around(&text, at, 60, 40)
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "name the console \"the Hetzner Console\" (https://console.hetzner.com), and say where \
         a token is created with cli_core::target::HETZNER_API_TOKENS_PAGE, \
         {HETZNER_API_TOKENS_PAGE:?}, word for word:\n{}",
        problems.join("\n")
    );
    for file in MUST_CARRY {
        assert!(
            carriers.iter().any(|c| c == file),
            "{file} carries the token page, but the scan did not find it there: it no longer \
             reads that kind of file"
        );
    }
    assert!(
        pending_hits > 0,
        "nothing left for PENDING_REMOVE_WARNING to exempt: the remove warning is aligned, so \
         delete that list and its use"
    );
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
    // A shell or YAML comment wrapped mid-name.
    assert_eq!(
        prose("# check the Hetzner Cloud\n# Console first"),
        "check the Hetzner Cloud Console first"
    );
    // A TypeScript string split with `+`, with either quote; any other `+` is left alone.
    assert_eq!(
        prose("'in the Hetzner Cloud ' +\n  'Console', a + b"),
        "'in the Hetzner Cloud Console', a + b"
    );
    assert_eq!(prose("\"in the \" + \"console\""), "\"in the console\"");
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

#[test]
fn old_console_names_finds_the_old_name_and_address_only() {
    for old in [
        "check the Hetzner Cloud Console.",
        "by ID in the Cloud Console and",
        "in the hetzner cloud console",
        "[Hetzner Cloud Console](https://console.hetzner.cloud):",
        "INSPECT https://console.hetzner.cloud AND DELETE",
        "Cloud Console → Security",
    ] {
        assert!(!old_console_names(old).is_empty(), "{old}");
    }
    for fine in [
        "the Hetzner Console (https://console.hetzner.com)",
        "https://api.hetzner.cloud/v1/servers",
        "https://docs.hetzner.cloud/#getting-started",
        "the Google Cloud Console",
        "Oracle Cloud Console",
        "a Hetzner-Console resize",
    ] {
        assert!(old_console_names(fine).is_empty(), "{fine}");
    }
}

#[test]
fn the_pending_exemption_covers_only_the_remove_warning() {
    let target_rs = "cli/platform-cli/src/commands/target.rs";
    let first = |s: &str| old_console_names(s)[0];
    for warning in [
        "or delete server 42 in the Hetzner Cloud Console.",
        "or delete server {id} in the Hetzner Cloud Console.",
        "fix them first if you mean to destroy that server, or delete it in the Hetzner Cloud \
         Console.",
    ] {
        assert!(pending(target_rs, warning, first(warning)), "{warning}");
        assert!(pending(
            "cli/platform-cli/tests/golden/target/remove_provisioned.golden",
            warning,
            first(warning)
        ));
        // The same sentence anywhere else is not pending.
        assert!(!pending(
            "cli/platform-cli/src/render/core_error.rs",
            warning,
            first(warning)
        ));
    }
    // Another sentence in a pending file is not pending either.
    for other in [
        "Check the Hetzner Cloud Console for the cause.",
        "delete this one's server by ID in the Cloud Console and run",
        "The only safe teardown of one of them is by ID in the Hetzner Cloud Console.",
    ] {
        assert!(
            !pending("docs/operator-guide/target-store.md", other, first(other)),
            "{other}"
        );
    }
}
