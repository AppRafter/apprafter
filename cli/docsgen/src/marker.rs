// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The `<!-- docs: … -->` marker: the one channel through which an
//! author tells the gate what a block is, and the one way to silence a
//! finding.
//!
//! Everything here is strict on purpose. An escape hatch becomes the
//! default the moment using it is cheaper than fixing the claim, so:
//!
//! * an unknown key is an error, not an ignored one — a typo'd
//!   `verfy=no` that silently means nothing is worse than no marker at
//!   all, because it reads to a reviewer as though it works;
//! * a duplicated key is an error rather than last-wins, for the same
//!   reason: `check=none check=cli` must not resolve quietly;
//! * `check=none` costs a **typed** reason from a closed vocabulary and
//!   a `since=` version, so exemptions can be counted by kind and aged
//!   (see [`age`]) rather than accumulating as untyped prose;
//! * a key that does not pair with the chosen check is an error, so
//!   `check=cue run=local` cannot look like it executes something.
//!
//! The marker is an HTML comment, which collides with nothing in the
//! corpus: 29 comments exist and 28 are the identical docsgen banner.
//! [`parse_marker`] returns `Ok(None)` — not an error — for every
//! comment that does not open with `docs:`, which is what keeps those
//! 28 invisible here.
//!
//! What this module does *not* do: decide what a marker means. It
//! parses and ages; the gate applies policy.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// What opens a marker, inside the comment. Anything else is not ours.
const PREFIX: &str = "docs:";

/// How long an exemption is good for, in days.
///
/// An exemption is a claim about the world ("this is third-party
/// output"), and the world moves. Re-justifying one is a minute's work;
/// inheriting one nobody has looked at in two years is how a gate ends
/// up guarding nothing.
pub const EXPIRY_DAYS: i64 = 180;

/// Which check the annotated block is subject to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Resolve the invocations in the block against the clap tree.
    Cli,
    /// Validate the block as a CUE document against the schemas.
    Cue,
    /// Check nothing — an exemption. Costs a [`Reason`] and a `since=`.
    None,
}

/// Why a block is exempt. A closed vocabulary, because the point of the
/// reason is to make exemptions *countable by kind*: "we have 14
/// third-party-output exemptions and 1 known-broken" is a fact a
/// reviewer can act on, while 15 free-text sentences are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Output of something we do not own — `kubectl get`, `helm ls`.
    ThirdPartyOutput,
    /// A deliberately incomplete snippet, shown to make one point.
    IllustrativeFragment,
    /// An invocation of a tool that is not `apprafter`.
    ExternalTool,
    /// The documented thing is wrong and tracked elsewhere. The most
    /// expensive reason to leave standing, and the one expiry is for.
    KnownBroken,
}

impl Reason {
    /// The spelling used in a marker, which is also how a report should
    /// name it — one spelling, so a grep for the marker text finds the
    /// report line and vice versa.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::ThirdPartyOutput => "third-party-output",
            Reason::IllustrativeFragment => "illustrative-fragment",
            Reason::ExternalTool => "external-tool",
            Reason::KnownBroken => "known-broken",
        }
    }
}

/// A parsed marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub check: Check,
    /// Execution mode. `Some("local")` means the invocation is run and
    /// its values and required arguments are checked too, not just its
    /// path and flag names. Only pairs with [`Check::Cli`].
    pub run: Option<String>,
    /// The schema kind to lay in for a CUE document that is not an
    /// `Application`. Only pairs with [`Check::Cue`].
    pub kind: Option<String>,
    /// The typed justification. Present exactly when [`Check::None`].
    pub reason: Option<Reason>,
    /// The release the exemption was taken in, `vMAJOR.MINOR.PATCH`.
    /// Present exactly when [`Check::None`]. [`age`] resolves it.
    pub since: Option<String>,
    /// The free text after the em dash, kept rather than dropped.
    ///
    /// It is the only human-readable part of an exemption, and a health
    /// report that lists exemptions without their sentence sends the
    /// reader back to grep the source of every one. The typed
    /// [`Reason`] is what the gate reasons over; this is what the
    /// reviewer reads.
    pub note: Option<String>,
}

/// Parse one line as a marker.
///
/// `Ok(None)` — not this module's business: the line is not a whole
/// HTML comment, or it is one that does not open with `docs:`.
/// `Err` — it *is* a marker and it is malformed. The distinction is the
/// whole point: a page may hold any comment it likes, but a comment
/// addressed to the gate must be well formed or the build stops.
pub fn parse_marker(line: &str) -> Result<Option<Marker>, String> {
    let trimmed = line.trim();
    let Some(inner) = trimmed
        .strip_prefix("<!--")
        .and_then(|body| body.strip_suffix("-->"))
    else {
        return Ok(None);
    };
    let Some(rest) = inner.trim().strip_prefix(PREFIX) else {
        return Ok(None);
    };

    // The em dash splits machine-readable fields from the sentence a
    // human wrote. A single character, chosen because it cannot occur
    // inside a key or a value, so the split needs no quoting rules.
    let (fields, note) = match rest.split_once('—') {
        Some((head, tail)) => (head, Some(tail.trim().to_string())),
        None => (rest, None),
    };

    let mut check: Option<Check> = None;
    let mut run: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut reason: Option<Reason> = None;
    let mut since: Option<String> = None;

    for token in fields.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            return Err(format!(
                "docs marker: `{token}` is not `key=value` (in `{trimmed}`)"
            ));
        };
        if value.is_empty() {
            return Err(format!(
                "docs marker: `{key}=` has no value (in `{trimmed}`)"
            ));
        }
        match key {
            "check" => set(&mut check, key, parse_check(value, trimmed)?, trimmed)?,
            "run" => set(&mut run, key, parse_run(value, trimmed)?, trimmed)?,
            "kind" => set(&mut kind, key, value.to_string(), trimmed)?,
            "reason" => set(&mut reason, key, parse_reason(value, trimmed)?, trimmed)?,
            "since" => set(&mut since, key, parse_since(value, trimmed)?, trimmed)?,
            _ => {
                return Err(format!(
                    "docs marker: unknown key `{key}` — known keys are \
                     check, run, kind, reason, since (in `{trimmed}`)"
                ))
            }
        }
    }

    let Some(check) = check else {
        return Err(format!(
            "docs marker: `check=` is required — one of cli, cue, none (in `{trimmed}`)"
        ));
    };

    // Which keys pair with which check. Stated as pairs rather than as
    // "ignored unless relevant" so a marker cannot read as doing
    // something it does not do.
    require(check == Check::None, reason.is_some(), "reason", trimmed)?;
    require(check == Check::None, since.is_some(), "since", trimmed)?;
    if check != Check::Cli && run.is_some() {
        return Err(format!(
            "docs marker: `run=` only pairs with check=cli (in `{trimmed}`)"
        ));
    }
    if check != Check::Cue && kind.is_some() {
        return Err(format!(
            "docs marker: `kind=` only pairs with check=cue (in `{trimmed}`)"
        ));
    }

    Ok(Some(Marker {
        check,
        run,
        kind,
        reason,
        since,
        note: note.filter(|n| !n.is_empty()),
    }))
}

/// Fill a slot once. A second assignment is an error rather than
/// last-wins: `check=none check=cli` must not resolve quietly.
fn set<T>(slot: &mut Option<T>, key: &str, value: T, line: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("docs marker: `{key}=` given twice (in `{line}`)"));
    }
    *slot = Some(value);
    Ok(())
}

/// Assert a key is present exactly when it is required.
fn require(required: bool, present: bool, key: &str, line: &str) -> Result<(), String> {
    match (required, present) {
        (true, false) => Err(format!(
            "docs marker: check=none requires `{key}=` — an exemption \
             must say why and since when (in `{line}`)"
        )),
        (false, true) => Err(format!(
            "docs marker: `{key}=` only pairs with check=none (in `{line}`)"
        )),
        _ => Ok(()),
    }
}

fn parse_check(value: &str, line: &str) -> Result<Check, String> {
    match value {
        "cli" => Ok(Check::Cli),
        "cue" => Ok(Check::Cue),
        "none" => Ok(Check::None),
        other => Err(format!(
            "docs marker: unknown check `{other}` — one of cli, cue, none (in `{line}`)"
        )),
    }
}

/// The execution vocabulary, closed for the same reason the reason
/// vocabulary is: a mode nobody implements would silently check less
/// than the author asked for, which is the quiet default this grammar
/// exists to prevent. Adding one is an edit here.
fn parse_run(value: &str, line: &str) -> Result<String, String> {
    match value {
        "local" => Ok(value.to_string()),
        other => Err(format!(
            "docs marker: unknown run mode `{other}` — only `local` (in `{line}`)"
        )),
    }
}

fn parse_reason(value: &str, line: &str) -> Result<Reason, String> {
    for reason in [
        Reason::ThirdPartyOutput,
        Reason::IllustrativeFragment,
        Reason::ExternalTool,
        Reason::KnownBroken,
    ] {
        if reason.as_str() == value {
            return Ok(reason);
        }
    }
    Err(format!(
        "docs marker: unknown reason `{value}` — one of third-party-output, \
         illustrative-fragment, external-tool, known-broken (in `{line}`)"
    ))
}

/// `since=` must name a release tag, `vMAJOR.MINOR.PATCH`.
///
/// Shape-checked here rather than left to [`age`], because a `since=`
/// that no tag can ever match is an exemption that can never expire —
/// and it would fail silently, as an unresolved lookup rather than a
/// malformed marker.
fn parse_since(value: &str, line: &str) -> Result<String, String> {
    let numeric = value.strip_prefix('v').is_some_and(|rest| {
        let parts: Vec<&str> = rest.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    });
    if numeric {
        Ok(value.to_string())
    } else {
        Err(format!(
            "docs marker: `since={value}` is not a release tag — \
             expected vMAJOR.MINOR.PATCH, e.g. v0.2.44 (in `{line}`)"
        ))
    }
}

/// How old an exemption is.
///
/// `since=` carries a **version**, not a date, so the age of an
/// exemption is not in the marker: it is in the repository, as the
/// commit the tag points at. That is the cheapest honest reading —
/// the alternative is a second date field the author must keep
/// consistent with the version they already wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Age {
    /// Whole days between the tagged commit and the moment asked about.
    Days(i64),
    /// The tag is not in this checkout, with git's reason. Two ordinary
    /// causes: the exemption names the release being prepared, whose
    /// tag does not exist yet (the spec's own example, `v0.2.45`, is
    /// one), or the clone was fetched without tags.
    ///
    /// **Not expired** — an absent tag is not evidence of an old
    /// exemption — but reported rather than swallowed, so a caller can
    /// say it could not age this one instead of passing it in silence.
    Unresolved(String),
}

impl Age {
    /// Whether the exemption has outlived [`EXPIRY_DAYS`].
    pub fn expired(&self) -> bool {
        matches!(self, Age::Days(days) if *days > EXPIRY_DAYS)
    }
}

/// Age the release named by `since=` against `now`.
///
/// `now` is a parameter rather than read inside, so the expiry rule can
/// be tested at a chosen instant instead of only on the days a test
/// happens to run.
pub fn age(since: &str, repo_root: &Path, now: SystemTime) -> Age {
    // Fully qualified: `v0.2.44` alone is ambiguous if a branch ever
    // shares the name, and git resolves that ambiguity with a warning
    // rather than an error.
    match commit_time(repo_root, &format!("refs/tags/{since}")) {
        Ok(tagged) => {
            let now = now
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Age::Days((now - tagged) / 86_400)
        }
        Err(why) => Age::Unresolved(why),
    }
}

/// The committer time, in Unix seconds, of whatever `rev` names.
fn commit_time(repo_root: &Path, rev: &str) -> Result<i64, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["log", "-1", "--format=%ct", rev])
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git could not resolve {rev}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    text.parse()
        .map_err(|e| format!("git returned {text:?} for {rev}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_line_that_is_not_a_whole_comment_is_not_ours() {
        assert_eq!(parse_marker("plain text").unwrap(), None);
        assert_eq!(parse_marker("<!-- docs: check=cli").unwrap(), None);
        assert_eq!(parse_marker("").unwrap(), None);
    }

    #[test]
    fn a_marker_with_no_check_is_an_error_not_an_empty_marker() {
        // It addressed the gate; it just did not say anything. Reading
        // that as "no annotation" would make `<!-- docs: -->` a way to
        // look annotated while being nothing.
        assert!(parse_marker("<!-- docs: -->").is_err());
        assert!(parse_marker("<!-- docs: run=local -->").is_err());
    }

    #[test]
    fn a_repeated_key_does_not_quietly_win() {
        assert!(parse_marker("<!-- docs: check=none check=cli -->").is_err());
        assert!(parse_marker("<!-- docs: check=cli run=local run=local -->").is_err());
    }

    #[test]
    fn a_bare_token_and_an_empty_value_are_errors() {
        assert!(parse_marker("<!-- docs: check=cli please -->").is_err());
        assert!(parse_marker("<!-- docs: check= -->").is_err());
    }

    #[test]
    fn keys_must_pair_with_their_check() {
        assert!(parse_marker("<!-- docs: check=cue run=local -->").is_err());
        assert!(parse_marker("<!-- docs: check=cli kind=Application -->").is_err());
        assert!(parse_marker("<!-- docs: check=cli since=v0.2.44 -->").is_err());
        assert!(parse_marker("<!-- docs: check=cli reason=external-tool -->").is_err());
    }

    #[test]
    fn the_note_after_the_em_dash_is_kept() {
        let m = parse_marker(
            "<!-- docs: check=none since=v0.2.44 reason=known-broken — tracked in 2.19d -->",
        )
        .unwrap()
        .unwrap();
        assert_eq!(m.note.as_deref(), Some("tracked in 2.19d"));
        assert_eq!(m.reason.map(Reason::as_str), Some("known-broken"));
        // A marker with nothing after the dash keeps no empty string.
        let bare = parse_marker("<!-- docs: check=cli -->").unwrap().unwrap();
        assert_eq!(bare.note, None);
    }

    #[test]
    fn since_must_name_a_release_tag() {
        for bad in ["yesterday", "0.2.44", "v0.2", "v0.2.x", "v0.2.44.1", "v"] {
            let line = format!("<!-- docs: check=none reason=external-tool since={bad} -->");
            assert!(
                parse_marker(&line).is_err(),
                "`since={bad}` can never resolve, so it can never expire"
            );
        }
    }

    #[test]
    fn expiry_fires_on_the_far_side_of_the_window() {
        assert!(!Age::Days(0).expired());
        assert!(!Age::Days(EXPIRY_DAYS).expired());
        assert!(Age::Days(EXPIRY_DAYS + 1).expired());
        assert!(!Age::Unresolved("no such tag".into()).expired());
    }

    /// The mechanism end to end, in a repository built for the test:
    /// an old tag ages past the window, a fresh one does not, and an
    /// absent one resolves to [`Age::Unresolved`]. Without this the
    /// expiry rule is arithmetic nobody has ever seen reach a tag.
    #[test]
    fn a_tag_ages_against_the_repository_that_holds_it() {
        let repo = tempfile::tempdir().unwrap();
        // Epoch-relative fixed instants, so the test asserts exact day
        // counts rather than "roughly".
        let old = 1_700_000_000i64;
        git(repo.path(), &["init", "-q"]);
        commit(repo.path(), "old", old);
        git(repo.path(), &["tag", "v0.1.0"]);
        commit(repo.path(), "new", old + 200 * 86_400);
        git(repo.path(), &["tag", "v0.2.0"]);

        let now = UNIX_EPOCH + Duration::from_secs((old + 200 * 86_400) as u64);
        assert_eq!(age("v0.1.0", repo.path(), now), Age::Days(200));
        assert!(age("v0.1.0", repo.path(), now).expired(), "200 > 180");
        assert_eq!(age("v0.2.0", repo.path(), now), Age::Days(0));
        assert!(!age("v0.2.0", repo.path(), now).expired());
        match age("v9.9.9", repo.path(), now) {
            Age::Unresolved(why) => assert!(why.contains("v9.9.9"), "{why}"),
            other => panic!("an absent tag must not age: {other:?}"),
        }
    }

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            // Hermetic: the developer's global config, hooks and
            // signing key must not decide whether this passes.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["-c", "user.name=docsgen", "-c", "user.email=docsgen@test"])
            .args(args)
            .output()
            .expect("git must be on PATH");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(repo: &Path, message: &str, at: i64) {
        let when = format!("{at} +0000");
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_DATE", &when)
            .env("GIT_COMMITTER_DATE", &when)
            .args(["-c", "user.name=docsgen", "-c", "user.email=docsgen@test"])
            .args([
                "commit",
                "-q",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                message,
            ])
            .output()
            .expect("git must be on PATH");
        assert!(
            out.status.success(),
            "git commit: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
