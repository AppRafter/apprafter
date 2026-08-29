// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The `recipe-purity` check, run over the real corpus.
//!
//! ADR 0058 asks for a gate built **red**: written against the pages as
//! they stand, before any of them is rewritten, so it must fail on real
//! historical text rather than on a synthetic case. This file is that
//! red state, expressed as assertions.
//!
//! It is expected to change as 2.20c rewrites pages. When a page named
//! below goes quiet, move it from the "still red" list to the "now
//! clean" list rather than deleting the assertion — a page that was
//! loud and is now silent is the evidence the restructure worked, and
//! an assertion that only ever gets deleted proves nothing.

use docsgen::gate::Gate;
use docsgen::recipe;

fn repo_root() -> std::path::PathBuf {
    // The test binary runs from the crate directory; the anchor is the
    // same one `docsgen::repo_root` uses.
    let mut dir = std::env::current_dir().expect("cwd");
    loop {
        if dir.join("cue.mod/module.cue").exists() {
            return dir;
        }
        assert!(dir.pop(), "no cue.mod/module.cue above the test's cwd");
    }
}

fn findings() -> Vec<docsgen::gate::Finding> {
    let root = repo_root();
    Gate::new(&root)
        .expect("build the gate")
        .recipe_findings()
        .expect("run the recipe check")
}

#[test]
fn the_check_is_red_on_the_corpus_today() {
    let found = findings();
    assert!(
        !found.is_empty(),
        "recipe-purity found nothing, which would mean the guides already \
         satisfy ADR 0058 — the census measured 325 foreign commands, so a \
         silent check is a broken check, not a clean corpus"
    );
}

#[test]
fn the_four_worst_pages_are_all_loud() {
    let found = findings();
    // The four pages the census named: each transcribes an e2e walk,
    // and they are the four highest foreign-command counts in the
    // corpus. If one of these goes quiet without its page being
    // rewritten, the check has lost surface.
    for page in [
        "docs/operator-guide/postgres.md",
        "docs/operator-guide/redis.md",
        "docs/operator-guide/persistent-disk.md",
        "docs/operator-guide/egress-policy.md",
    ] {
        let count = found.iter().filter(|f| f.file == page).count();
        assert!(count > 0, "{page} should be red and is not");
    }
}

#[test]
fn a_clean_page_is_silent() {
    // The negative control, and the assertion that matters most: a
    // checker that flagged every page would satisfy every test above.
    // The census measured these at zero foreign commands.
    let found = findings();
    for page in [
        "docs/operator-guide/choosing-the-machine.md",
        "docs/operator-guide/cloudflare-origin-cert.md",
        "docs/dev-guide/environments.md",
        "docs/dev-guide/private-repos-and-registries.md",
    ] {
        let hits: Vec<_> = found
            .iter()
            .filter(|f| f.file == page)
            .map(|f| format!("{}:{} {}", f.file, f.line, f.message))
            .collect();
        assert!(
            hits.is_empty(),
            "{page} carries no foreign commands and must stay silent, got:\n{}",
            hits.join("\n")
        );
    }
}

#[test]
fn a_break_glass_page_is_never_reported() {
    let found = findings();
    for (page, reason) in recipe::BREAK_GLASS_PAGES {
        let count = found.iter().filter(|f| &f.file == page).count();
        assert_eq!(count, 0, "{page} is exempt ({reason}) but was reported");
    }
    // And the exemption is load-bearing rather than decorative: the
    // rescue runbook really does carry foreign commands, so a table
    // that stopped being consulted would show up here.
    let root = repo_root();
    let rescue = root.join("docs/operator-guide/recovery.md");
    let source = std::fs::read_to_string(&rescue).expect("read the rescue runbook");
    let mut carries_foreign = false;
    for line in source.lines() {
        if !recipe::foreign_commands(line).is_empty() {
            carries_foreign = true;
            break;
        }
    }
    assert!(
        carries_foreign,
        "recovery.md no longer carries a foreign command, so its break-glass \
         exemption is now silencing nothing and should be retired"
    );
}

#[test]
fn only_platform_tools_are_reported() {
    // Every finding today names a tool AppRafter could plausibly have
    // replaced. A new name appearing here is either a real regression
    // in a guide or a gap in the allowlist, and both want a human.
    let found = findings();
    let mut names: Vec<String> = found
        .iter()
        .filter_map(|f| {
            f.message
                .split('`')
                .nth(1)
                .map(std::string::ToString::to_string)
        })
        .collect();
    names.sort();
    names.dedup();
    assert_eq!(
        names,
        vec!["hubble".to_string(), "kubectl".to_string()],
        "the reported tools changed; if this is a new guide reaching for a \
         new tool, decide whether it is allowlisted or a finding"
    );
}
