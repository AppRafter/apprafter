// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Resolve a documented `apprafter …` invocation against the clap tree.
//!
//! # What is checked, and why only that
//!
//! **The command path and the flag names. Never a value, never whether
//! a required argument is present.** That narrowing is forced by the
//! corpus, not by taste:
//!
//! * 38 in-scope invocations are a *usage synopsis*, not shell —
//!   `[--namespace <ns> ...]`, `[--keep-daily N]`,
//!   `[--reprovision | --data-only]`. Correct documentation that any
//!   shell tokenizer rejects.
//! * `|` is alternation as often as it is a pipe — `<pg|redis|disk>`
//!   and `monolithic|sequential` against two genuine
//!   `apprafter kubeconfig | tee` pipelines. The two do not separate
//!   lexically without bracket awareness, so [`segments`] has it.
//! * 161 of the 213 `apprafter …` inline spans (76 %) are a bare
//!   command path — no flags and no arguments at all. Measured over
//!   the corpus as **found**, at `dc4c5de`, the commit this check was
//!   written against; the ratio has barely moved since (165 of 219 at
//!   the time of writing, 75 %). Demanding arguments on those would
//!   fail every one, including inside the reference this crate itself
//!   generates — a gate that fails its own output teaches contributors
//!   to disable it.
//!
//! Path and flag name still catch every confirmed drift in the corpus:
//! a command that never shipped (`apprafter promote`), and a flag
//! borrowed from a sibling — `--namespace` is real on twelve commands
//! and absent on `app status` and `app remove`, which is exactly the
//! shape a reader copies and a reviewer misses. (`platform fork` and
//! `platform channel`, the other known-deferred names, appear only in
//! ADRs, changelogs and `platform-stack/README.md` — none of which
//! [`crate::scan`] admits, so no check here can reach them.)
//!
//! # What is not checked, and cannot be
//!
//! `requires` / `conflicts` are `pub(crate)` in this clap version with
//! no getter, so "these two flags are mutually exclusive" is out of
//! reach — a known gap, recorded here rather than approximated.
//! Argument *arity* is out of scope by the same reasoning as required
//! arguments: a documented path is a reference to a command, not a
//! runnable line.
//!
//! # Where this sits
//!
//! [`crate::scan`] decides what text is a claim; this module decides
//! whether a claim is true. Keeping them apart is what lets the corpus
//! shapes above be tested as *lexing* questions with no clap tree in
//! the room.

use crate::model;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::path::Path;

/// The binary name an invocation must open with. A segment that starts
/// with anything else is somebody else's command.
const BINARY: &str = "apprafter";

/// The clap projection, indexed for resolution: every node by path, and
/// every parent's children by *token* — canonical name and each alias
/// alike.
///
/// Aliases are indexed per parent rather than globally because they are
/// only unique there: `ls` is the alias of the `list` under `app`,
/// `backup`, `migration`, `repo creds`, `target`, `target domain` and
/// `volume`, and `rm` names three different `remove`s. A flat alias map
/// would have to pick one of seven meanings for `ls`; a per-parent one
/// never faces the question.
/// `an_alias_is_unique_among_its_siblings` pins that this is the only
/// ambiguity the real tree has.
pub struct Tree {
    nodes: Vec<model::CommandNode>,
    /// Index of the node with the empty path. It is where `apprafter
    /// --version` resolves, and where [`Tree::global_args`] reads the
    /// one genuinely global flag from.
    root: usize,
    /// Parallel to `nodes`: `(token, child index)` for every child of
    /// that node, under its name and under each of its aliases.
    children: Vec<Vec<(String, usize)>>,
}

impl Tree {
    /// Read a committed `commands.json`.
    pub fn from_commands_json(path: &Path) -> Result<Tree, Box<dyn Error>> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let model: model::Tree = serde_json::from_str(&text)
            .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
        Tree::from_model(model)
    }

    /// Index an in-memory projection — the form the gate uses when it
    /// resolves against the live clap tree instead of the committed
    /// JSON.
    pub fn from_model(model: model::Tree) -> Result<Tree, Box<dyn Error>> {
        let nodes = model.commands;
        let index: HashMap<&[String], usize> = nodes
            .iter()
            .enumerate()
            .map(|(at, node)| (node.path.as_slice(), at))
            .collect();
        let root = *index.get(&[] as &[String]).ok_or(
            "the projection has no root node (empty path): global flags \
             would have nothing to resolve against",
        )?;

        let mut children: Vec<Vec<(String, usize)>> = vec![Vec::new(); nodes.len()];
        for (at, node) in nodes.iter().enumerate() {
            let Some((name, parent)) = node.path.split_last() else {
                continue;
            };
            // An orphan cannot occur in a projection built by
            // `model::tree_from`, which walks parents into children;
            // ignoring one keeps a hand-edited JSON from panicking the
            // gate it is meant to feed.
            let Some(&up) = index.get(parent) else {
                continue;
            };
            children[up].push((name.clone(), at));
            for alias in &node.aliases {
                children[up].push((alias.clone(), at));
            }
        }

        Ok(Tree {
            nodes,
            root,
            children,
        })
    }

    /// The child of `at` reached by `token` — its name or any alias.
    fn child(&self, at: usize, token: &str) -> Option<usize> {
        self.children[at]
            .iter()
            .find(|(name, _)| name == token)
            .map(|(_, child)| *child)
    }

    fn has_children(&self, at: usize) -> bool {
        !self.children[at].is_empty()
    }

    /// How to spell the node in a diagnostic: `apprafter app status`.
    fn spelling(&self, at: usize) -> String {
        let mut out = String::from(BINARY);
        for part in &self.nodes[at].path {
            out.push(' ');
            out.push_str(part);
        }
        out
    }

    /// The flags every command accepts, wherever they are written.
    ///
    /// Exactly one pair: clap's generated `--help` / `-h`. [`model`]
    /// filters it off every node but the root — it is documented once,
    /// globally, and the root is that one place — so a resolver that
    /// did not consult the root would reject `apprafter app list
    /// --help`.
    ///
    /// **`--version` is not global**, though it sits beside `--help` on
    /// the root node. clap generates it for the root alone, and the
    /// only other command that has one is `apprafter platform freeze`,
    /// which declares its own. Chaining the root's whole arg list here
    /// made `apprafter app list --version` resolve — a documented line
    /// the shipped binary answers with `error: unexpected argument
    /// '--version' found`, which is the false-published-example class
    /// this guard exists to close.
    ///
    /// Derived from the root rather than written out, so it cannot
    /// claim a flag the CLI no longer has;
    /// `the_root_declares_the_help_flag_this_resolver_treats_as_global`
    /// in `invocation_test.rs` refuses to let it come back empty.
    fn global_args(&self) -> impl Iterator<Item = &model::ArgSpec> {
        self.nodes[self.root]
            .args
            .iter()
            .filter(|arg| arg.long.as_deref() == Some("help"))
    }

    /// Whether `flag` names a real flag of `at` — or a global one.
    fn knows_flag(&self, at: usize, flag: &str) -> bool {
        let here = &self.nodes[at].args;
        if let Some(long) = flag.strip_prefix("--") {
            let long = long.split('=').next().unwrap_or(long);
            return here
                .iter()
                .chain(self.global_args())
                .any(|arg| arg.long.as_deref() == Some(long));
        }
        let Some(shorts) = flag.strip_prefix('-') else {
            return false;
        };
        let shorts = shorts.split('=').next().unwrap_or(shorts);
        // A single-dash token may bundle switches (`-rf`), which clap
        // accepts, so every character has to name a short.
        !shorts.is_empty()
            && shorts.chars().all(|c| {
                here.iter()
                    .chain(self.global_args())
                    .any(|arg| arg.short == Some(c))
            })
    }
}

/// One documented command line, reduced to the two things checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Every non-flag token after the binary, in order — subcommands
    /// and positional values alike. Which is which is a question only
    /// the tree can answer, so [`resolve`] decides it, not [`extract`].
    pub path: Vec<String>,
    /// Flag tokens as written, `--name` / `-n`, without any `=value`.
    pub flags: Vec<String>,
    /// The segment this came from, for the report line.
    pub raw: String,
}

/// Why an invocation does not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// A token in command position names no subcommand or alias of a
    /// command that has subcommands.
    UnknownCommand { token: String, at: String },
    /// A flag the command does not declare — most often one borrowed
    /// from a sibling that does.
    UnknownFlag { flag: String, at: String },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::UnknownCommand { token, at } => {
                write!(f, "`{at}` has no subcommand `{token}`")
            }
            ResolveError::UnknownFlag { flag, at } => {
                write!(f, "`{at}` has no flag `{flag}`")
            }
        }
    }
}

impl Error for ResolveError {}

/// Every `apprafter` invocation on one line of documentation.
///
/// The line is a *documentation* line, so this is deliberately not a
/// shell parse: it is the smallest reading that separates the command
/// being documented from the prose, notation and other tools around
/// it.
///
/// * a `#` that opens a comment ends the line — `apprafter up  # alias:
///   apprafter bootstrap-all` documents one invocation, not two;
/// * one level of `$( … )` and of backticks is unwrapped, because
///   `export KUBECONFIG="$(apprafter kubeconfig --refresh)"` is how
///   four walkthroughs open;
/// * the line splits on `;`, `&&`, `||` and a top-level `|`, and an
///   invocation may start only at a segment's start (after an optional
///   `$ ` prompt). That last rule is what keeps `sudo mv apprafter
///   /usr/local/bin/` and `cargo run --bin apprafter -- app list` out:
///   neither *runs* `apprafter`. It also means `watch apprafter
///   platform status` is not seen — an accepted blind spot, and the
///   cheapest place to be wrong, since a missed line is a line the
///   gate simply does not judge.
///
/// Results come back in source order, substitutions included.
pub fn extract(line: &str) -> Vec<Invocation> {
    let mut found: Vec<(usize, Invocation)> = Vec::new();
    collect(line, 0, &mut found);
    found.sort_by_key(|(at, _)| *at);
    found
        .into_iter()
        .map(|(_, invocation)| invocation)
        .collect()
}

/// Join backslash continuations in a block body, keeping each logical
/// line's *first* physical line number (0-based within the body).
///
/// Not a nicety: 18 invocations in the corpus put the command on one
/// line and its flags on the next, so a strictly per-line check would
/// see `apprafter app add …` with no flags at all — and judge the flags
/// it exists to judge on none of them.
pub fn logical_lines(body: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut open: Option<(usize, String)> = None;

    for (index, raw) in body.lines().enumerate() {
        let trimmed = raw.trim_end();
        let (text, continues) = match trimmed.strip_suffix('\\') {
            Some(head) => (head.trim_end(), true),
            None => (trimmed, false),
        };
        match open.as_mut() {
            Some((_, joined)) => {
                joined.push(' ');
                joined.push_str(text.trim_start());
            }
            None => open = Some((index, text.to_string())),
        }
        if !continues {
            out.extend(open.take());
        }
    }
    // A body whose last line ends in a backslash still has content.
    out.extend(open.take());
    out
}

/// Extract from `text`, reporting positions relative to `base`.
fn collect(text: &str, base: usize, out: &mut Vec<(usize, Invocation)>) {
    let text = strip_comment(text);
    let (masked, inner) = unwrap_substitutions(text);
    for (at, segment) in segments(&masked) {
        // Tokenised from the MASK and reported from the source. Mask
        // and source share byte offsets, so both slices are the same
        // region — but a substitution's own flags must not be charged
        // to the outer command: tokenising the source made `python -c`
        // inside `--token "$(python -c …)"` read as `-c` on `apprafter
        // target add`, a flag that command has never had.
        if let Some(invocation) = invocation_of(segment, &text[at..at + segment.len()]) {
            out.push((base + at, invocation));
        }
    }
    for (at, sub) in inner {
        collect(&sub, base + at, out);
    }
}

/// The line up to the `#` that opens a comment, if one does.
///
/// A `#` counts only at a token boundary and outside quotes, so a URL
/// fragment and a `'pass#word'` literal survive while
/// `# alias: apprafter bootstrap-all` — a whole line of prose in a
/// fence — is removed entirely.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut after_space = true;
    for (at, ch) in line.char_indices() {
        match quote {
            Some(open) => {
                if ch == open {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '#' if after_space => return &line[..at],
                _ => {}
            },
        }
        after_space = ch.is_whitespace();
    }
    line
}

/// Blank out every `$( … )` and backtick substitution, and hand back
/// what was inside each with its offset.
///
/// The mask is space-for-byte so offsets in it are offsets in the
/// source; the caller relies on that to report a real position and to
/// slice the untouched segment back out.
fn unwrap_substitutions(text: &str) -> (String, Vec<(usize, String)>) {
    let mut masked = String::with_capacity(text.len());
    let mut inner = Vec::new();
    let mut at = 0;

    while at < text.len() {
        let rest = &text[at..];
        if let Some(close) = rest
            .starts_with("$(")
            .then(|| matching_paren(rest.as_bytes(), 1))
            .flatten()
        {
            inner.push((at + 2, rest[2..close].to_string()));
            blank(&mut masked, close + 1);
            at += close + 1;
            continue;
        }
        if let Some(after) = rest.strip_prefix('`') {
            if let Some(close) = after.find('`').map(|found| found + 1) {
                inner.push((at + 1, rest[1..close].to_string()));
                blank(&mut masked, close + 1);
                at += close + 1;
                continue;
            }
        }
        let ch = rest.chars().next().expect("at < len, so a char follows");
        masked.push(ch);
        at += ch.len_utf8();
    }
    (masked, inner)
}

/// Index of the `)` closing the `(` at `open`, nesting respected.
fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (at, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

fn blank(out: &mut String, bytes: usize) {
    for _ in 0..bytes {
        out.push(' ');
    }
}

/// Split a line into command segments: `(offset, text)`.
///
/// `;`, `&&` and `||` always separate. A single `|` separates only when
/// it is *spaced* and outside `[ ]` — the two forms measured in the
/// corpus separate perfectly on that rule: alternation is written
/// tight (`<pg|redis|disk>`) or bracketed
/// (`[--reprovision | --data-only]`), a pipeline spaced and bare
/// (`apprafter kubeconfig | tee /tmp/kc`). Getting this wrong is not
/// cosmetic: splitting an alternation strands `--data-only]` in a
/// segment of its own, where it is no longer anybody's flag, and
/// merging a pipeline hands `grep`'s flags to `apprafter`.
fn segments(line: &str) -> Vec<(usize, &str)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut at = 0;
    let mut quote: Option<u8> = None;
    let mut brackets = 0usize;

    while at < bytes.len() {
        let byte = bytes[at];
        if let Some(open) = quote {
            if byte == open {
                quote = None;
            }
            at += 1;
            continue;
        }
        let width = match byte {
            b'\'' | b'"' => {
                quote = Some(byte);
                at += 1;
                continue;
            }
            b'[' => {
                brackets += 1;
                at += 1;
                continue;
            }
            b']' => {
                brackets = brackets.saturating_sub(1);
                at += 1;
                continue;
            }
            b';' => 1,
            b'&' if bytes.get(at + 1) == Some(&b'&') => 2,
            b'|' if bytes.get(at + 1) == Some(&b'|') => 2,
            b'|' if brackets == 0 && at > 0 && bytes[at - 1].is_ascii_whitespace() => 1,
            _ => {
                at += 1;
                continue;
            }
        };
        out.push((start, &line[start..at]));
        at += width;
        start = at;
    }
    out.push((start, &line[start..]));
    out
}

/// The invocation a segment holds, if it opens one. `segment` is the
/// masked text that is read; `raw` is the same region of the source,
/// which is what a report has to show.
fn invocation_of(segment: &str, raw: &str) -> Option<Invocation> {
    let trimmed = segment.trim();
    let mut tokens = trimmed.split_whitespace();
    let mut first = tokens.next()?;
    // A `console` fence writes the prompt; it is not part of the command.
    if first == "$" {
        first = tokens.next()?;
    }
    if first != BINARY {
        return None;
    }

    let mut invocation = Invocation {
        path: Vec::new(),
        flags: Vec::new(),
        raw: raw.trim().to_string(),
    };
    for token in tokens {
        match classify(token) {
            Token::Flag(flag) => invocation.flags.push(flag),
            Token::Value(value) => invocation.path.push(value),
            Token::Ignored => {}
        }
    }
    Some(invocation)
}

enum Token {
    Flag(String),
    Value(String),
    Ignored,
}

/// What one whitespace-separated token is.
///
/// Synopsis brackets are notation *around* a token, not part of it, so
/// they come off first: `[--namespace` is the flag `--namespace` and
/// `--data-only]` is `--data-only`. Dropping bracketed tokens whole
/// instead would make every optional flag in a synopsis unjudged, which
/// is 38 lines of the corpus and the densest flag surface in the docs.
fn classify(token: &str) -> Token {
    let token = token.trim_matches(|c| c == '[' || c == ']');
    let Some(first) = token.chars().next() else {
        return Token::Ignored;
    };
    match first {
        // A placeholder, a shell variable, or a quoted value — none of
        // them can name a subcommand or a flag.
        '<' | '$' | '\'' | '"' => return Token::Ignored,
        // Shell punctuation, and the `\` a caller that did not join
        // continuations leaves behind.
        '>' | '|' | '&' | '\\' => return Token::Ignored,
        _ => {}
    }
    if token.chars().all(|c| c == '.' || c == '…' || c == '*') {
        // `...`, `…` and `*`: "more of the same" and "any of them".
        // `apprafter app *` names the command family, which is a true
        // claim about a real branch of the tree; only the leaf is left
        // unsaid, and an unsaid leaf cannot drift.
        return Token::Ignored;
    }
    let Some(rest) = token.strip_prefix('-') else {
        return Token::Value(token.to_string());
    };
    // Bare `-` is stdin and `--` is end-of-options; a leading digit is
    // a negative number, which is a value however it is spelled.
    if rest.is_empty()
        || rest == "-"
        || rest.starts_with(|c: char| c.is_ascii_digit())
        || rest.trim_start_matches('-').is_empty()
    {
        return Token::Ignored;
    }
    let name = token.split('=').next().unwrap_or(token);
    Token::Flag(name.to_string())
}

/// Walk an invocation's path, then check its flag names.
///
/// The walk is greedy and stops at the first token that names no child:
/// from there on the tokens are positional values, and values are not
/// this gate's business. Whether stopping is *legal* is what the node
/// decides — a command with subcommands has no positionals anywhere in
/// this tree (asserted by `every_branch_node_is_a_pure_branch`), so an
/// unmatched token there is a command that does not exist, while at a
/// leaf it is simply an argument.
///
/// A token may also name *several* siblings at once:
/// `apprafter repo creds add/list/show` and `apprafter migration
/// list/approve/reject` are how four pages introduce a command family.
/// Every alternative must name a real sibling — `add/frobnicate` still
/// fails on `frobnicate` — so the notation costs the gate nothing;
/// the walk simply carries more than one candidate from there on, and
/// a flag is accepted if any candidate declares it.
pub fn resolve(tree: &Tree, invocation: &Invocation) -> Result<(), ResolveError> {
    resolve_to(tree, invocation).map(|_| ())
}

/// [`resolve`], handing back the command path(s) the invocation landed
/// on — canonical, so an alias comes back spelled out (`apprafter t
/// use` resolves to `["target", "use"]`).
///
/// More than one path comes back only for the `add/list/show`
/// alternation [`resolve`] documents; the order is the order the
/// alternatives were written.
///
/// This exists for one caller — [`crate::examples::check`], which must
/// know not just that an example resolves but WHICH command it
/// resolves to, because an example filed under `backup enable` that
/// invokes `app list` is a true invocation and a false page. Splitting
/// it out of [`resolve`] rather than writing a second walk is
/// deliberate: two resolvers in one gate eventually disagree, which
/// this track has already lived through with fence scanners.
pub fn resolve_to<'a>(
    tree: &'a Tree,
    invocation: &Invocation,
) -> Result<Vec<&'a [String]>, ResolveError> {
    let mut at = vec![tree.root];

    for token in &invocation.path {
        match step(tree, &at, token) {
            Step::Descend(next) => at = next,
            // A positional value: everything after it is a value too.
            Step::Stop => break,
            Step::Unknown(token) => {
                return Err(ResolveError::UnknownCommand {
                    token,
                    at: tree.spelling(at[0]),
                })
            }
        }
    }

    for flag in &invocation.flags {
        if !at.iter().any(|&node| tree.knows_flag(node, flag)) {
            return Err(ResolveError::UnknownFlag {
                flag: flag.clone(),
                at: tree.spelling(at[0]),
            });
        }
    }
    Ok(at
        .into_iter()
        .map(|node| tree.nodes[node].path.as_slice())
        .collect())
}

/// What one path token does to the candidate set.
enum Step {
    Descend(Vec<usize>),
    Stop,
    Unknown(String),
}

fn step(tree: &Tree, at: &[usize], token: &str) -> Step {
    let direct: Vec<usize> = at
        .iter()
        .filter_map(|&node| tree.child(node, token))
        .collect();
    if !direct.is_empty() {
        return Step::Descend(direct);
    }
    // Only now consider the token an alternation — a command name is
    // never spelled with a slash, so this cannot shadow a real one.
    if token.contains('/') {
        let mut all = Vec::new();
        for alternative in token.split('/') {
            let hit: Vec<usize> = at
                .iter()
                .filter_map(|&node| tree.child(node, alternative))
                .collect();
            if hit.is_empty() {
                // Name the alternative that failed, not the whole
                // token: `…/reject` is the part a reader must fix.
                return unresolved(tree, at, alternative);
            }
            all.extend(hit);
        }
        return Step::Descend(all);
    }
    unresolved(tree, at, token)
}

/// A token that named nothing: an unknown command where subcommands
/// were possible, a positional value where they were not.
fn unresolved(tree: &Tree, at: &[usize], token: &str) -> Step {
    if at.iter().any(|&node| tree.has_children(node)) {
        Step::Unknown(token.to_string())
    } else {
        Step::Stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comment_ends_the_line_only_at_a_token_boundary() {
        assert_eq!(strip_comment("apprafter up  # alias"), "apprafter up  ");
        assert_eq!(strip_comment("# whole line"), "");
        assert_eq!(strip_comment("https://x#frag"), "https://x#frag");
        assert_eq!(
            strip_comment("seal --from-literal 'a#b'"),
            "seal --from-literal 'a#b'"
        );
    }

    #[test]
    fn a_spaced_top_level_pipe_splits_and_alternation_does_not() {
        assert_eq!(segments("a | b").len(), 2);
        assert_eq!(segments("<pg|redis|disk>").len(), 1);
        assert_eq!(segments("[--reprovision | --data-only]").len(), 1);
        assert_eq!(segments("a && b ; c").len(), 3);
    }

    #[test]
    fn brackets_come_off_a_token_but_the_flag_inside_stays() {
        assert!(matches!(classify("[--namespace"), Token::Flag(f) if f == "--namespace"));
        assert!(matches!(classify("--data-only]"), Token::Flag(f) if f == "--data-only"));
        assert!(matches!(classify("--from-literal=k=v"), Token::Flag(f) if f == "--from-literal"));
        assert!(matches!(classify("<ns>"), Token::Ignored));
        assert!(matches!(classify("..."), Token::Ignored));
        assert!(matches!(classify("--"), Token::Ignored));
        assert!(matches!(classify("-1"), Token::Ignored));
        assert!(matches!(classify("status"), Token::Value(v) if v == "status"));
    }

    #[test]
    fn a_continuation_joins_and_keeps_the_first_line_number() {
        let joined = logical_lines("apprafter app add \\\n  --name web\nsecond\n");
        assert_eq!(
            joined,
            vec![
                (0, "apprafter app add --name web".to_string()),
                (2, "second".to_string())
            ]
        );
    }
}
