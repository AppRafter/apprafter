// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Is a foreign command sitting in a guide's recipe path? (ADR 0058)
//!
//! # The rule
//!
//! A guide is a recipe. Its main flow carries `apprafter` commands and
//! genuinely external tools; everything else routes by role — walk
//! material deleted, independent verification collapsed into a
//! disclosure, mechanism to its own page, failure handling to the
//! troubleshooting catalogue. So a foreign command is a finding unless
//! it is on [`ALLOWLIST`], inside a collapsed disclosure, or on a
//! [`BREAK_GLASS_PAGES`] page.
//!
//! # Fenced and literal blocks only
//!
//! Inline spans are **out of scope**, and that is a decision rather
//! than an oversight. A fence body is shell by construction, so reading
//! its first token as a command word is sound. Prose is not: over the
//! guide corpus the same reading applied to spans returns the first
//! word of the sentence, and a gate that reports `the` as an
//! unauthorised command is a gate contributors switch off. The cost is
//! a real gap — a guide can put `kubectl get pods` in a prose span and
//! this check will not see it. Named here rather than approximated; if
//! the restructure finds it matters, it gets closed with a measurement
//! behind it, the way [`crate::invocation`] chose its own narrowing.
//!
//! # Tags exclude, they never select
//!
//! [`crate::scan`] warns that hand-written shell in this corpus carries
//! four different info strings, so a tag-keyed gate would see a
//! fraction of the surface. That warning is about the **inclusive**
//! direction. [`NON_SHELL_TAGS`] uses the exclusive one: a fence tagged
//! `cue` or `mermaid` is definitively not shell, and skipping it can
//! only lose findings, never invent them. Measured need — over the 27
//! guide pages the un-excluded reading returns `package` and `import`
//! from CUE, `flowchart` from mermaid, and `apps`, `default`, `target`,
//! `server` and `sync` from YAML: 30-odd inventions against 155 real
//! `kubectl` calls.

use crate::invocation::segment_heads;

/// External tools ADR 0058 permits in a recipe path, each with why it
/// is here. A flat table carrying its own evidence, in the style of
/// [`crate::shipped`]: adding a row is the review point.
///
/// The bar is "AppRafter cannot own this". Installing the binary
/// happens before the binary exists; `git` and a container build push
/// to somebody else's forge and registry; a rescue ramdisk has no
/// cluster and no `apprafter`; DNS and TLS live outside the platform;
/// `restic` is the documented portability escape hatch, so a repository
/// stays readable without us; and a shell's own configuration is the
/// reader's, not ours.
pub const ALLOWLIST: &[(&str, &str)] = &[
    // Pre-install bootstrap — runs before `apprafter` is on the machine.
    (
        "curl",
        "fetches the release artefact before the binary exists",
    ),
    ("wget", "the curl alternative in the same bootstrap"),
    ("tar", "unpacks the release artefact"),
    ("gh", "the GitHub CLI bootstrap tab"),
    ("cargo", "builds the binary from source"),
    ("sudo", "places the unpacked binary on PATH"),
    ("mv", "places the unpacked binary on PATH"),
    ("cp", "coreutils in the bootstrap and the rescue ramdisk"),
    (
        "mkdir",
        "creates the reader's own shell-completion directory",
    ),
    (
        "rm",
        "coreutils; removing a file the platform does not manage",
    ),
    ("chmod", "coreutils in the bootstrap"),
    // Somebody else's forge and registry.
    (
        "git",
        "the application repository is the reader's, on their forge",
    ),
    (
        "docker",
        "the image is built and pushed to the reader's registry",
    ),
    ("podman", "the rootless container alternative"),
    ("buildah", "the daemonless build alternative"),
    // Rescue ramdisk: no cluster, no apprafter binary, no state.
    ("ssh", "the reader is in a provider rescue ramdisk"),
    (
        "ssh-keygen",
        "host-key hygiene for the reader's own known_hosts",
    ),
    ("lsblk", "util-linux inside the rescue ramdisk"),
    ("mount", "mounting the original root from a foreign ramdisk"),
    (
        "umount",
        "unmounting before the console re-enables normal boot",
    ),
    ("swapoff", "the manual finish for a failed swap rollback"),
    ("systemctl", "host service control on the node itself"),
    ("journalctl", "host log reading on the node itself"),
    // Outside the platform by nature.
    ("dig", "DNS resolution is the registrar's and the CDN's"),
    ("host", "the dig alternative"),
    ("nslookup", "the dig alternative"),
    (
        "openssl",
        "certificate inspection is not a platform operation",
    ),
    (
        "restic",
        "the documented portability escape hatch: a repository \
       stays readable with no AppRafter at all",
    ),
    (
        "vault",
        "the reader's own secret store, feeding a provider token into \
         the environment before any AppRafter command runs",
    ),
    (
        "loginctl",
        "ends the reader's own login session so a host group membership \
         takes effect; their machine, not the platform",
    ),
    // The reader's own shell.
    ("export", "shell builtin: the reader's environment"),
    ("source", "shell builtin: the reader's environment"),
    ("cd", "shell builtin"),
    ("exit", "shell builtin"),
    ("autoload", "zsh builtin, in the reader's own rc"),
    ("compinit", "zsh completion wiring, in the reader's own rc"),
    ("fpath", "zsh completion path, in the reader's own rc"),
    ("bash", "running a shipped script"),
    ("sh", "running a shipped script"),
    ("zsh", "running a shipped script"),
    // Reading and shaping output. These are pipeline stages, never the
    // operation: the head of `kubectl get … | jq …` is still kubectl.
    ("echo", "prints; not an operation on the platform"),
    ("cat", "reads a local file"),
    ("printf", "prints"),
    ("grep", "filters output"),
    ("sed", "filters output"),
    ("awk", "filters output"),
    ("jq", "filters JSON output"),
    ("yq", "filters YAML output"),
    ("head", "filters output"),
    ("tail", "filters output"),
    ("sort", "filters output"),
    ("uniq", "filters output"),
    ("wc", "counts output"),
    ("tr", "filters output"),
    ("cut", "filters output"),
    ("xargs", "shapes a pipeline"),
    ("tee", "shapes a pipeline"),
    ("base64", "decodes a value the previous stage produced"),
    ("find", "locates a local file"),
    ("ls", "lists a local directory"),
    (
        "timeout",
        "bounds another command; the bounded one is the head \
       that matters and this check reads it from the next segment",
    ),
    ("sleep", "waits"),
    ("date", "prints the time"),
    ("watch", "repeats another command"),
    ("true", "shell builtin"),
    ("false", "shell builtin"),
    // Contributor recipes, which a guide may legitimately cite.
    ("just", "the repository's own task runner"),
    ("nix", "the pinned toolchain"),
    ("bun", "the JavaScript workspace"),
    ("cue", "the schema tool the platform ships against"),
];

/// Pages exempt wholesale, each with the reason. Anything added here is
/// the review point: a page-level exemption says this page is not a
/// recipe, which is a claim about what the page is for.
pub const BREAK_GLASS_PAGES: &[(&str, &str)] = &[
    (
        "docs/operator-guide/recovery.md",
        "the reader is inside a provider rescue ramdisk: the cluster is \
         down by definition and the apprafter binary is not installed, \
         so no CLI command can run in this context at all",
    ),
    (
        "docs/operator-guide/troubleshooting.md",
        "a diagnostic catalogue, which is exactly where ADR 0058 routes \
         failure handling — the page IS the destination",
    ),
];

/// Info strings that are definitively not shell. Excluding only; see
/// the module docs on why the inclusive direction is forbidden and this
/// one is not.
const NON_SHELL_TAGS: &[&str] = &[
    "cue",
    "yaml",
    "yml",
    "json",
    "toml",
    "ini",
    "mermaid",
    "text",
    "txt",
    "output",
    "diff",
    "patch",
    "dockerfile",
    "rust",
    "go",
    "ts",
    "tsx",
    "js",
    "python",
    "py",
    "sql",
    "hcl",
    "xml",
    "html",
    "css",
    "make",
];

/// Whether a fence's info string means its body is not shell.
pub fn is_non_shell_tag(tag: Option<&str>) -> bool {
    tag.is_some_and(|t| NON_SHELL_TAGS.contains(&t))
}

/// Whether `name` is allowlisted.
pub fn is_allowed(name: &str) -> bool {
    ALLOWLIST.iter().any(|(tool, _)| *tool == name)
}

/// The break-glass reason for `path`, if it has one.
pub fn break_glass_reason(path: &str) -> Option<&'static str> {
    BREAK_GLASS_PAGES
        .iter()
        .find(|(page, _)| *page == path)
        .map(|(_, reason)| *reason)
}

/// Whether a token can plausibly be a program name.
///
/// Deliberately narrow. Everything this rejects is something the check
/// would otherwise invent a command out of: a colon-terminated word
/// opens a line of command OUTPUT (`fatal: repository not found`), a
/// dotted or slashed token is a field path or a file path, an
/// upper-case token is a CRD kind or an environment variable, and a
/// long hyphenated token is a resource name (`platform-redis-ephemeral-
/// 000-admin`) rather than a binary. A missed foreign command is a line
/// the gate does not judge; an invented one is noise that gets the
/// whole class switched off.
fn looks_like_a_program(token: &str) -> bool {
    let plausible_length = (2..=16).contains(&token.chars().count());
    let shape = token
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    let opens_with_a_letter = token.starts_with(|c: char| c.is_ascii_lowercase());
    let hyphens = token.matches('-').count();
    plausible_length && shape && opens_with_a_letter && hyphens <= 2
}

/// Every foreign command on one line of a shell block.
///
/// A head counts when it plausibly names a program, is not `apprafter`,
/// is not allowlisted, and does not stand alone — a bare word on its
/// own line is output, not an invocation.
pub fn foreign_commands(line: &str) -> Vec<String> {
    let heads = segment_heads(line);
    if heads.is_empty() {
        return Vec::new();
    }
    // A segment with no arguments is not an invocation being taught.
    // Counting tokens on the whole line rather than per segment keeps
    // this cheap and errs toward silence.
    if line.split_whitespace().count() < 2 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for head in heads {
        if head == "apprafter" || is_allowed(&head) || !looks_like_a_program(&head) {
            continue;
        }
        if !out.contains(&head) {
            out.push(head);
        }
    }
    out
}

/// 1-based inclusive line ranges covered by a collapsed disclosure.
///
/// A MkDocs-Material collapsible block opens with `???` or `???+` and
/// holds everything indented under it. `!!!` — the always-open
/// admonition — is deliberately **not** a disclosure: ADR 0058's rule
/// is that the material is collapsed, not merely set apart, so an
/// admonition that is open by default leaves the reader's happy path
/// exactly as interrupted as a bare fence would.
///
/// Computed from the raw source rather than from [`crate::scan`]'s
/// blocks: containment is a source-line question, `Block` carries no
/// indentation, and scan has many other consumers to keep stable.
pub fn disclosure_ranges(source: &str) -> Vec<(usize, usize)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    let mut at = 0usize;

    while at < lines.len() {
        let Some(opener_indent) = disclosure_opener_indent(lines[at]) else {
            at += 1;
            continue;
        };
        let start = at + 1; // 1-based
        let mut end = start;
        let mut probe = at + 1;
        while probe < lines.len() {
            let line = lines[probe];
            if line.trim().is_empty() {
                // A blank line does not end the block, but it does not
                // extend it either: a disclosure followed by two blank
                // lines and then prose must not swallow the prose.
                probe += 1;
                continue;
            }
            if indent_of(line) <= opener_indent {
                break;
            }
            end = probe + 1;
            probe += 1;
        }
        out.push((start, end));
        at = probe;
    }
    out
}

/// The indent of a `???` / `???+` opener, or `None` for any other line.
fn disclosure_opener_indent(line: &str) -> Option<usize> {
    let indent = indent_of(line);
    let rest = line.trim_start();
    let rest = rest.strip_prefix("???")?;
    // `???` and `???+` both open one; `????` is not a thing.
    let rest = rest.strip_prefix('+').unwrap_or(rest);
    // A bare `???` with nothing after it is prose, not an admonition.
    rest.starts_with(char::is_whitespace).then_some(indent)
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Whether `line` (1-based) falls inside any disclosure.
pub fn inside_disclosure(ranges: &[(usize, usize)], line: usize) -> bool {
    ranges
        .iter()
        .any(|(start, end)| line >= *start && line <= *end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_commands_finds_kubectl() {
        assert_eq!(
            foreign_commands("kubectl get pods -n demo"),
            vec!["kubectl"]
        );
    }

    #[test]
    fn foreign_commands_passes_apprafter_and_the_allowlist() {
        assert!(foreign_commands("apprafter app list").is_empty());
        assert!(foreign_commands("git push -u origin main").is_empty());
        assert!(foreign_commands("restic -r /repo snapshots").is_empty());
    }

    #[test]
    fn foreign_commands_reads_a_pipeline_head_not_its_filters() {
        // The operation is the kubectl call; jq and base64 shape what
        // it printed.
        assert_eq!(
            foreign_commands("kubectl get secret s -o json | jq -r .data.x | base64 -d"),
            vec!["kubectl"]
        );
    }

    #[test]
    fn foreign_commands_ignores_command_output() {
        // `fatal: repository not found` is a git error, not a command.
        assert!(foreign_commands("fatal: repository not found").is_empty());
        // A resource name printed by a previous command.
        assert!(foreign_commands("platform-redis-ephemeral-000-admin   Opaque   2").is_empty());
    }

    #[test]
    fn foreign_commands_ignores_a_bare_word() {
        assert!(foreign_commands("kubectl").is_empty());
    }

    #[test]
    fn foreign_commands_finds_the_dependency_shells() {
        assert_eq!(foreign_commands("psql -c 'select 1'"), vec!["psql"]);
        assert_eq!(
            foreign_commands("redis-cli -u $URL PING"),
            vec!["redis-cli"]
        );
        assert_eq!(foreign_commands("helm list -n argocd"), vec!["helm"]);
    }

    #[test]
    fn non_shell_tags_exclude_the_measured_inventions() {
        for tag in ["cue", "yaml", "mermaid", "json", "text"] {
            assert!(is_non_shell_tag(Some(tag)), "{tag} must be excluded");
        }
        // The four tags hand-written shell actually carries.
        for tag in ["sh", "bash", "console"] {
            assert!(!is_non_shell_tag(Some(tag)), "{tag} is shell");
        }
        assert!(!is_non_shell_tag(None), "an untagged fence may be shell");
    }

    #[test]
    fn disclosure_ranges_covers_an_indented_fence() {
        let source = "\
# Page

??? note \"Verify independently\"

    ```sh
    kubectl get pods
    ```

Back to prose.
";
        let ranges = disclosure_ranges(source);
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        // The fence opens on line 5 and closes on line 7.
        assert!(inside_disclosure(&ranges, 5), "{ranges:?}");
        assert!(inside_disclosure(&ranges, 7), "{ranges:?}");
        // The prose after it is outside.
        assert!(!inside_disclosure(&ranges, 9), "{ranges:?}");
    }

    #[test]
    fn disclosure_ranges_does_not_merge_two_blocks() {
        let source = "\
??? note \"One\"

    kubectl a

Prose between.

??? note \"Two\"

    kubectl b
";
        let ranges = disclosure_ranges(source);
        assert_eq!(ranges.len(), 2, "{ranges:?}");
        assert!(
            !inside_disclosure(&ranges, 5),
            "prose is outside: {ranges:?}"
        );
    }

    #[test]
    fn an_open_admonition_is_not_a_disclosure() {
        // `!!!` renders expanded, so it interrupts the happy path just
        // as a bare fence does. ADR 0058 asks for collapsed.
        let source = "!!! note \"Open\"\n\n    kubectl get pods\n";
        assert!(disclosure_ranges(source).is_empty());
    }

    #[test]
    fn a_bare_question_run_is_not_a_disclosure() {
        assert!(disclosure_ranges("???\n\n    kubectl a\n").is_empty());
    }

    #[test]
    fn every_break_glass_page_states_a_reason() {
        for (page, reason) in BREAK_GLASS_PAGES {
            assert!(!reason.trim().is_empty(), "{page} needs a reason");
            assert!(page.starts_with("docs/"), "{page} must be repo-relative");
        }
    }

    #[test]
    fn every_allowlist_entry_states_a_reason() {
        for (tool, reason) in ALLOWLIST {
            assert!(!reason.trim().is_empty(), "{tool} needs a reason");
            assert!(
                looks_like_a_program(tool),
                "{tool} would never be seen as a command anyway"
            );
        }
    }
}
