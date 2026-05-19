// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Semantic color helpers for `apprafter` CLI output.
//!
//! Wraps `owo-colors` so the rest of the codebase emits intent
//! (`style::ok("…")`, `style::fail("…")`) rather than concrete
//! colors. Three reasons to centralise this:
//!
//! 1. **Consistency.** Same semantic role (success / warning /
//!    failure / hint / dim) → same color across `bootstrap-all`,
//!    `doctor`, `target list`, `whoami`, etc. A future rebrand
//!    flips a single file, not every callsite.
//! 2. **`NO_COLOR` respect.** `owo-colors` 4.x with the
//!    `supports-colors` feature evaluates `NO_COLOR` (and stdout
//!    TTY-ness) lazily on each format call. Set `NO_COLOR=1` or
//!    pipe stdout to a file and every helper here returns the
//!    raw `Display` text with zero ANSI bytes — no callsite-by-
//!    callsite opt-out needed.
//! 3. **Tests.** Unit tests can assert behaviour at the helper
//!    boundary (raw vs colored) without spawning subprocesses.

use owo_colors::{OwoColorize, Stream};

/// Foreground green — successful checks, `✓` markers, "verified"
/// labels in `whoami`/`doctor`. Honours `NO_COLOR`.
pub fn ok(t: &str) -> String {
    format!("{}", t.if_supports_color(Stream::Stdout, |t| t.green()))
}

/// Foreground yellow — `doctor` WARN rows, soft failures (token
/// unverified but format-valid, etc.).
pub fn warn(t: &str) -> String {
    format!("{}", t.if_supports_color(Stream::Stdout, |t| t.yellow()))
}

/// Foreground red — `doctor` FAIL rows, `✗ FAILED after Ns`
/// markers from `bootstrap-all`, terminal errors that bubble out
/// before miette renders them. Uses `Stream::Stderr` because the
/// callsites that consume `fail()` write to stderr.
pub fn fail(t: &str) -> String {
    format!("{}", t.if_supports_color(Stream::Stderr, |t| t.red()))
}

/// Foreground cyan — neutral status / phase markers (the `→` in
/// `bootstrap-all`, column headers in `target list`, the
/// "(active)" tag in `whoami`/`target show`). Reads as
/// "informational accent, not a result".
pub fn info(t: &str) -> String {
    format!("{}", t.if_supports_color(Stream::Stdout, |t| t.cyan()))
}

/// Dim grey — tertiary annotations (`(from --flag)` hints in
/// `target add` wizard, `(unset — apply uses platform-1)` defaults
/// in `bootstrap-all --dry-run`). Pulls attention AWAY without
/// hiding the text.
pub fn dim(t: &str) -> String {
    format!("{}", t.if_supports_color(Stream::Stdout, |t| t.dimmed()))
}

/// Bold emphasis — target names, cluster names, identity strings
/// in `whoami`. Doesn't change the color; combine with
/// `info()`/`ok()`/etc. by wrapping the bolded text:
/// `info(&bold("dev"))`.
pub fn bold(t: &str) -> String {
    format!("{}", t.if_supports_color(Stream::Stdout, |t| t.bold()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `owo-colors` 4.x with the `supports-colors` feature
    /// suppresses ANSI when the stream isn't a TTY. In `cargo
    /// test` stdout is captured, so the helpers return ANSI-free
    /// text. This is the "off" case — pin it as the default
    /// behaviour under tests so regressions in colour-stripping
    /// surface here, not in a real terminal.
    fn no_ansi(s: &str) -> bool {
        !s.contains('\x1b')
    }

    #[test]
    fn ok_returns_ansi_free_text_when_stream_is_not_a_tty() {
        let s = ok("PASS");
        assert!(no_ansi(&s), "expected no ANSI under non-TTY, got {s:?}");
        assert!(s.contains("PASS"));
    }

    #[test]
    fn warn_fail_info_dim_bold_all_round_trip_text_under_no_tty() {
        for (label, rendered) in [
            ("WARN", warn("WARN")),
            ("FAIL", fail("FAIL")),
            ("INFO", info("INFO")),
            ("dim", dim("dim")),
            ("bold", bold("bold")),
        ] {
            assert!(
                no_ansi(&rendered),
                "expected no ANSI for {label}: {rendered:?}"
            );
            assert!(
                rendered.contains(label),
                "expected literal `{label}` to survive: {rendered:?}"
            );
        }
    }
}
