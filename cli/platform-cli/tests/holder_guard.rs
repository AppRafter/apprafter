// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The holder's source rules (D.4a §3.10; each mutation-tested):
//!
//! - **HG1** — nothing under `src/holder/` names `Instant`, `SystemTime` or `UNIX_EPOCH`: a
//!   window is measured on clocks that count sleep (`window::SystemClock`, T56), and `Instant`
//!   does not (holder finding 5).
//! - **HG2** — one way in and one way to spawn (T47; Task 5).
//! - **HG3** — the pipe's and the client's security flags (T53; Task 7).
//! - **HG5** — the two secret-bearing frame kinds, `Kind::Init` and `Kind::Secret`, are named only
//!   by the codec's six functions (S6).
//!
//! Tokens (`proc_macro2`), as G1 reads them: a comment never counts, attributes (doc comments
//! included) are skipped, and a hit is attributed to the nearest enclosing `fn`. A string
//! literal is no identifier.

use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, TokenStream, TokenTree};

/// HG1.
const BANNED_CLOCKS: [&str; 3] = ["Instant", "SystemTime", "UNIX_EPOCH"];
/// HG5: the secret-bearing kinds, as path segments after `::`.
const SECRET_KINDS: [&str; 2] = ["Init", "Secret"];
/// HG5: the functions that may name them.
const SECRET_KIND_FNS: [&str; 6] = [
    "write_init",
    "read_init",
    "write_secret",
    "read_reply",
    "from_byte",
    "carries_secret",
];

/// One identifier or string literal of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    line: usize,
    /// The nearest enclosing `fn`.
    within: Option<String>,
    /// `::` right before it.
    after_colons: bool,
    /// The identifier, or the literal's text with its quotes.
    text: String,
    literal: bool,
}

fn walk(tokens: TokenStream, within: Option<&str>, out: &mut Vec<Seen>) {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    let mut next_fn: Option<String> = None;
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            TokenTree::Punct(p) if p.as_char() == '#' => {
                i += match (tokens.get(i + 1), tokens.get(i + 2)) {
                    (Some(TokenTree::Group(g)), _) if g.delimiter() == Delimiter::Bracket => 2,
                    (Some(TokenTree::Punct(bang)), Some(TokenTree::Group(g)))
                        if bang.as_char() == '!' && g.delimiter() == Delimiter::Bracket =>
                    {
                        3
                    }
                    _ => 1,
                };
                continue;
            }
            TokenTree::Punct(p) if p.as_char() == ';' => next_fn = None,
            TokenTree::Group(g) => {
                if g.delimiter() == Delimiter::Brace {
                    if let Some(name) = next_fn.take() {
                        walk(g.stream(), Some(&name), out);
                        i += 1;
                        continue;
                    }
                }
                walk(g.stream(), within, out);
            }
            TokenTree::Ident(id) => {
                let name = id.to_string();
                if name == "fn" {
                    if let Some(TokenTree::Ident(f)) = tokens.get(i + 1) {
                        next_fn = Some(f.to_string());
                        i += 2;
                        continue;
                    }
                }
                let after_colons = i >= 2
                    && matches!((&tokens[i - 1], &tokens[i - 2]),
                        (TokenTree::Punct(a), TokenTree::Punct(b)) if a.as_char() == ':' && b.as_char() == ':');
                out.push(Seen {
                    line: id.span().start().line,
                    within: within.map(str::to_string),
                    after_colons,
                    text: name,
                    literal: false,
                });
            }
            TokenTree::Literal(lit) => out.push(Seen {
                line: lit.span().start().line,
                within: within.map(str::to_string),
                after_colons: false,
                text: lit.to_string(),
                literal: true,
            }),
            _ => {}
        }
        i += 1;
    }
}

fn seen(src: &str) -> Vec<Seen> {
    let tokens: TokenStream = src.parse().expect("the source tokenizes");
    let mut out = Vec::new();
    walk(tokens, None, &mut out);
    out
}

fn cli_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("platform-cli sits in cli/")
        .to_path_buf()
}

/// Every `.rs` file under `dir`, as (path under `cli/`, `/`-separated; source).
fn sources(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap_or_else(|e| panic!("{d:?}: {e}")) {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                let rel = p
                    .strip_prefix(cli_dir())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, fs::read_to_string(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn holder_sources() -> Vec<(String, String)> {
    let files = sources(&cli_dir().join("platform-cli/src/holder"));
    assert!(
        files.iter().any(|(f, _)| f.ends_with("holder/protocol.rs")),
        "the holder's sources are where this guard looks: {files:?}"
    );
    files
}

/// HG1's hits in one source.
fn clock_hits(src: &str) -> Vec<Seen> {
    seen(src)
        .into_iter()
        .filter(|s| !s.literal && BANNED_CLOCKS.contains(&s.text.as_str()))
        .collect()
}

/// HG5's hits in one source.
fn secret_kind_hits(src: &str) -> Vec<Seen> {
    seen(src)
        .into_iter()
        .filter(|s| {
            !s.literal
                && s.after_colons
                && SECRET_KINDS.contains(&s.text.as_str())
                && !s
                    .within
                    .as_deref()
                    .is_some_and(|f| SECRET_KIND_FNS.contains(&f))
        })
        .collect()
}

#[test]
fn hg1_nothing_under_holder_names_a_clock_that_skips_sleep() {
    let hits: Vec<String> = holder_sources()
        .iter()
        .flat_map(|(f, src)| {
            clock_hits(src)
                .into_iter()
                .map(move |s| format!("{f}:{}: {}", s.line, s.text))
        })
        .collect();
    assert!(
        hits.is_empty(),
        "a holder window must not use Instant or SystemTime (they stop in suspend or jump; \
         use window::SystemClock, D.4a T56):\n{}",
        hits.join("\n")
    );
}

#[test]
fn hg5_only_the_codec_names_the_secret_bearing_kinds() {
    let mut hits = Vec::new();
    let mut in_codec = 0;
    for (f, src) in holder_sources() {
        for s in seen(&src) {
            if !s.literal && s.after_colons && SECRET_KINDS.contains(&s.text.as_str()) {
                in_codec += 1;
            }
        }
        hits.extend(
            secret_kind_hits(&src)
                .into_iter()
                .map(|s| format!("{f}:{}: ::{} in {:?}", s.line, s.text, s.within)),
        );
    }
    assert!(
        in_codec >= 4,
        "the codec names the two kinds itself ({in_codec} found)"
    );
    assert!(
        hits.is_empty(),
        "Kind::Init and Kind::Secret carry the data key and an opened item: only {SECRET_KIND_FNS:?} \
         may name them, so every secret byte goes through their exact-size buffers (S6):\n{}",
        hits.join("\n")
    );
}

/// HG2 (T47): `HOLDER_ARG` is named only where it is defined, where the holder is spawned and
/// where `main` dispatches on it; `run_holder` only where it is defined and in `main`; the
/// literal `__holder` only in the definition; `current_exe` beside `HOLDER_ARG` only in the
/// spawn — one way in, one way to start a holder.
const ARG_FILES: [&str; 3] = [
    "platform-cli/src/holder/mod.rs",
    "platform-cli/src/holder/spawn.rs",
    "platform-cli/src/main.rs",
];
const RUN_FILES: [&str; 2] = ["platform-cli/src/holder/mod.rs", "platform-cli/src/main.rs"];
const SPAWN_FILE: &str = "platform-cli/src/holder/spawn.rs";

fn hg2_hits(file: &str, src: &str) -> Vec<String> {
    let s = seen(src);
    let names = |n: &str| s.iter().any(|x| !x.literal && x.text == n);
    let mut hits = Vec::new();
    for x in &s {
        if !x.literal && x.text == "HOLDER_ARG" && !ARG_FILES.contains(&file) {
            hits.push(format!("{file}:{}: HOLDER_ARG", x.line));
        }
        if !x.literal && x.text == "run_holder" && !RUN_FILES.contains(&file) {
            hits.push(format!("{file}:{}: run_holder", x.line));
        }
        if x.literal && x.text.contains("__holder") && file != "platform-cli/src/holder/mod.rs" {
            hits.push(format!("{file}:{}: the literal {}", x.line, x.text));
        }
    }
    if names("current_exe") && names("HOLDER_ARG") && file != SPAWN_FILE {
        hits.push(format!("{file}: current_exe beside HOLDER_ARG"));
    }
    hits
}

#[test]
fn hg2_one_way_in_and_one_way_to_start_a_holder() {
    let files = sources(&cli_dir().join("platform-cli/src"));
    let src = |f: &str| {
        seen(
            &files
                .iter()
                .find(|(p, _)| p == f)
                .unwrap_or_else(|| panic!("{f} exists"))
                .1,
        )
    };
    let main = src("platform-cli/src/main.rs");
    assert!(
        main.iter().any(|x| x.text == "HOLDER_ARG") && main.iter().any(|x| x.text == "run_holder"),
        "main dispatches on HOLDER_ARG"
    );
    let spawn = src(SPAWN_FILE);
    assert!(
        spawn.iter().any(|x| x.text == "current_exe")
            && spawn.iter().any(|x| x.text == "HOLDER_ARG"),
        "the spawn starts this binary as the holder"
    );
    let hits: Vec<String> = files.iter().flat_map(|(f, s)| hg2_hits(f, s)).collect();
    assert!(
        hits.is_empty(),
        "a second way into or out to the holder (D.4a HG2):\n{}",
        hits.join("\n")
    );
}

#[test]
fn hg2_sees_a_second_spawn_and_a_second_dispatch() {
    assert_eq!(
        hg2_hits(
            "platform-cli/src/x.rs",
            "fn f() { let _ = (std::env::current_exe(), HOLDER_ARG); }"
        )
        .len(),
        2
    );
    assert_eq!(
        hg2_hits("platform-cli/src/y.rs", r#"fn g() { h("__holder") }"#).len(),
        1
    );
    // A message that spells the argument, inside a macro, outside `holder/mod.rs`: the
    // servers print `NOT_A_COMMAND` instead (critic-d4a-e finding 2).
    assert_eq!(
        hg2_hits(
            "platform-cli/src/holder/server/unix.rs",
            r#"fn run() { eprintln!("error: `__holder` is not a command") }"#
        )
        .len(),
        1
    );
    assert!(hg2_hits(
        "platform-cli/src/holder/server/unix.rs",
        r#"fn run() { eprintln!("{}", crate::holder::NOT_A_COMMAND) }"#
    )
    .is_empty());
    assert!(hg2_hits(
        "platform-cli/src/main.rs",
        "fn main() { if a == HOLDER_ARG { run_holder() } }"
    )
    .is_empty());
}

/// The scanner itself, on a source of its own.
#[test]
fn the_scanner_sees_aliases_paths_and_enclosing_functions() {
    let src = r#"
use std::time::Instant as Clock;
fn a() { let _ = Clock::now(); }
fn write_init() { let _ = Kind::Init; }
fn other() { let k = Kind::Secret; /* Kind::Init */ let _ = "Kind::Init Instant"; }
#[doc = "Instant"] fn d() {}
impl X { fn from_byte() -> Kind { Self::Secret } }
"#;
    let clocks: Vec<usize> = clock_hits(src).iter().map(|s| s.line).collect();
    assert_eq!(clocks, vec![2]);
    let kinds: Vec<(usize, Option<String>)> = secret_kind_hits(src)
        .into_iter()
        .map(|s| (s.line, s.within))
        .collect();
    assert_eq!(kinds, vec![(5, Some("other".to_string()))]);
}
