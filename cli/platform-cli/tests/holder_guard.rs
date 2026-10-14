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
//! - **HG6** — each reader of the window's two clocks makes the OS call its clock names (T56, O4):
//!   on a machine that never slept the two read alike, so the source is what tells them apart.
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

/// The identifiers inside the argument list of each call of `callee` in `src`.
fn call_args(src: &str, callee: &str) -> Vec<Vec<String>> {
    fn idents(tokens: TokenStream, out: &mut Vec<String>) {
        for t in tokens {
            match t {
                TokenTree::Ident(id) => out.push(id.to_string()),
                TokenTree::Group(g) => idents(g.stream(), out),
                _ => {}
            }
        }
    }
    fn walk(tokens: TokenStream, callee: &str, out: &mut Vec<Vec<String>>) {
        let t: Vec<TokenTree> = tokens.into_iter().collect();
        for (i, tt) in t.iter().enumerate() {
            match tt {
                TokenTree::Ident(id) if *id == callee => {
                    if let Some(TokenTree::Group(g)) = t.get(i + 1) {
                        if g.delimiter() == Delimiter::Parenthesis {
                            let mut args = Vec::new();
                            idents(g.stream(), &mut args);
                            out.push(args);
                        }
                    }
                }
                TokenTree::Group(g) => walk(g.stream(), callee, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(src.parse().expect("the source tokenizes"), callee, &mut out);
    out
}

/// HG3 (T53): the holder's pipe is created first-instance-only and closed to remote clients; its
/// client opens it at identification level, so a squatter can never act as the person.
#[test]
fn hg3_the_pipe_and_its_client_carry_their_security_flags() {
    let files = holder_sources();
    let src = |f: &str| {
        files
            .iter()
            .find(|(p, _)| p == f)
            .unwrap_or_else(|| panic!("{f}"))
            .1
            .clone()
    };
    let pipe = call_args(
        &src("platform-cli/src/holder/server/windows.rs"),
        "CreateNamedPipeW",
    );
    assert_eq!(pipe.len(), 1, "one CreateNamedPipeW: {pipe:?}");
    for flag in [
        "FILE_FLAG_FIRST_PIPE_INSTANCE",
        "PIPE_REJECT_REMOTE_CLIENTS",
    ] {
        assert!(
            pipe[0].iter().any(|a| a == flag),
            "CreateNamedPipeW without {flag}: {:?}",
            pipe[0]
        );
    }
    let open = call_args(&src("platform-cli/src/holder/client.rs"), "CreateFileW");
    assert_eq!(open.len(), 1, "one CreateFileW: {open:?}");
    for flag in ["SECURITY_SQOS_PRESENT", "SECURITY_IDENTIFICATION"] {
        assert!(
            open[0].iter().any(|a| a == flag),
            "CreateFileW without {flag}: {:?}",
            open[0]
        );
    }
}

#[test]
fn hg3_reads_the_arguments_of_a_call() {
    let args = call_args(
        "fn f() { let _ = CreateFileW(a, B | C, g(D)); }",
        "CreateFileW",
    );
    assert_eq!(
        args,
        vec![["a", "B", "C", "g", "D"].map(String::from).to_vec()]
    );
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

// ---------------------------------------------------------------------------------------------
// HG6 (T56, O4): each of the window's clocks reads the OS clock it names
// ---------------------------------------------------------------------------------------------

/// HG6: each reader of the window's two clocks, as it must read, in the file it lives in (the
/// clocks `window::HolderClock`'s docs name). `SystemClock` asks `os::current`, the running OS's
/// module; each OS's `clock_with_sleep` reads the clock that counts suspend and its
/// `clock_awake` the one that does not; on Unix both go through `os::clock`, which reads the
/// clock it is given. On a machine that has not slept the two clocks read alike, so only the
/// source tells a swap apart; `window.rs`'s wiring tests prove, per OS, that `SystemClock`'s
/// readings are those OS clocks. Every OS's reader is checked here on every OS.
const CLOCK_READERS: [(&str, &str); 11] = [
    (
        "platform-cli/src/holder/window.rs",
        "impl HolderClock for SystemClock {
             fn with_sleep(&self) -> Duration { super::os::current::clock_with_sleep() }
             fn awake_only(&self) -> Duration { super::os::current::clock_awake() }
         }",
    ),
    (
        "platform-cli/src/holder/os/mod.rs",
        "#[cfg(target_os = \"linux\")] pub(crate) use linux as current;
         #[cfg(target_os = \"macos\")] pub(crate) use macos as current;
         #[cfg(windows)] pub(crate) use windows as current;",
    ),
    (
        "platform-cli/src/holder/os/mod.rs",
        "pub(crate) fn clock(id: libc::clockid_t) -> std::time::Duration {
             let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0, };
             unsafe { libc::clock_gettime(id, &mut ts) };
             std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
         }",
    ),
    (
        "platform-cli/src/holder/os/linux.rs",
        "pub(crate) const WITH_SLEEP: libc::clockid_t = libc::CLOCK_BOOTTIME;",
    ),
    (
        "platform-cli/src/holder/os/linux.rs",
        "pub(crate) const AWAKE: libc::clockid_t = libc::CLOCK_MONOTONIC;",
    ),
    (
        "platform-cli/src/holder/os/linux.rs",
        "pub(crate) fn clock_with_sleep() -> Duration { super::clock(WITH_SLEEP) }
         pub(crate) fn clock_awake() -> Duration { super::clock(AWAKE) }",
    ),
    (
        "platform-cli/src/holder/os/macos.rs",
        "pub(crate) const WITH_SLEEP: libc::clockid_t = libc::CLOCK_MONOTONIC_RAW;",
    ),
    (
        "platform-cli/src/holder/os/macos.rs",
        "pub(crate) const AWAKE: libc::clockid_t = libc::CLOCK_UPTIME_RAW;",
    ),
    (
        "platform-cli/src/holder/os/macos.rs",
        "pub(crate) fn clock_with_sleep() -> Duration { super::clock(WITH_SLEEP) }
         pub(crate) fn clock_awake() -> Duration { super::clock(AWAKE) }",
    ),
    (
        "platform-cli/src/holder/os/windows.rs",
        "pub(crate) fn clock_with_sleep() -> Duration {
             Duration::from_millis(unsafe { GetTickCount64() })
         }",
    ),
    (
        "platform-cli/src/holder/os/windows.rs",
        "pub(crate) fn clock_awake() -> Duration {
             let mut t = 0u64;
             unsafe { QueryUnbiasedInterruptTime(&mut t) };
             Duration::from_nanos(t.saturating_mul(100))
         }",
    ),
];

/// `src` as one line of tokens with a space before and after each: comments are gone, doc
/// comments are attributes between items, and a layout or a comment changes nothing.
fn token_line(src: &str) -> String {
    fn flat(tokens: TokenStream, out: &mut String) {
        for t in tokens {
            match t {
                TokenTree::Group(g) => {
                    let (open, close) = match g.delimiter() {
                        Delimiter::Parenthesis => ("(", ")"),
                        Delimiter::Brace => ("{", "}"),
                        Delimiter::Bracket => ("[", "]"),
                        Delimiter::None => ("", ""),
                    };
                    out.push_str(open);
                    out.push(' ');
                    flat(g.stream(), out);
                    out.push_str(close);
                    out.push(' ');
                }
                other => {
                    out.push_str(&other.to_string());
                    out.push(' ');
                }
            }
        }
    }
    let mut out = String::from(" ");
    flat(src.parse().expect("the source tokenizes"), &mut out);
    out
}

/// How many times `item`'s tokens run, whole and in order, in `src`'s.
fn item_count(src: &str, item: &str) -> usize {
    token_line(src).matches(&token_line(item)).count()
}

#[test]
fn hg6_each_window_clock_reads_the_os_clock_it_names() {
    let wrong: Vec<String> = CLOCK_READERS
        .iter()
        .filter_map(|(file, item)| {
            let src = fs::read_to_string(cli_dir().join(file)).unwrap();
            let n = item_count(&src, item);
            (n != 1).then(|| format!("{file}: found {n} times, once expected:\n{item}"))
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "a reader of the window's clocks no longer makes the OS call its clock names (D.4a T56, \
         O4: the clock that counts suspend, and the one that does not, read apart):\n{}",
        wrong.join("\n\n")
    );
}

/// HG6's reading: a layout, a comment or a doc comment changes nothing; another token does,
/// and so does a token that only starts or ends like the one expected.
#[test]
fn hg6_reads_an_item_as_its_tokens() {
    let src = "/// Reads it.\npub(crate) fn f() -> D {\n    // SAFETY: none.\n    g(A)\n}\nfn h() { g(AB) }\n";
    assert_eq!(item_count(src, "pub(crate) fn f() -> D { g(A) }"), 1);
    assert_eq!(item_count(src, "pub(crate) fn f() -> D { g(B) }"), 0);
    assert_eq!(item_count(src, "g(A)"), 1);
    assert_eq!(item_count(src, "g(AB)"), 1);
    assert_eq!(item_count(src, "A"), 1, "AB is not A");
    assert_eq!(item_count(src, "B"), 0);
}
