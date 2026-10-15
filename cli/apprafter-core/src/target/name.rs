// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The target-name rule both clients apply, with the CLI's reason texts.

/// The longest target name, in bytes.
pub const TARGET_NAME_MAX_LEN: usize = 64;

/// Why a target name is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameProblem {
    Empty,
    /// Longer than [`TARGET_NAME_MAX_LEN`] bytes; `len` is the name's.
    TooLong {
        len: usize,
    },
    /// A character other than an ASCII letter, digit or `-`.
    InvalidChar,
    /// Starts or ends with `-`.
    EdgeDash,
}

impl NameProblem {
    /// The snake_case name the desktop's error fields carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooLong { .. } => "too_long",
            Self::InvalidChar => "invalid_char",
            Self::EdgeDash => "edge_dash",
        }
    }

    /// The CLI's `check_target_name` text for `name`, byte for byte (two of the texts embed
    /// the name, so this is not a `Display`).
    pub fn reason(self, name: &str) -> String {
        match self {
            Self::Empty => "target name must not be empty".to_string(),
            Self::TooLong { len } => {
                format!("target name must be ≤ {TARGET_NAME_MAX_LEN} chars (got {len})")
            }
            Self::InvalidChar => {
                format!("target name `{name}` is invalid — allowed: alphanumeric + `-`")
            }
            Self::EdgeDash => format!("target name `{name}` must not start or end with `-`"),
        }
    }
}

/// The CLI's target-name rule, in its order: empty, length (bytes), characters, edges. The
/// characters match Kubernetes resource names, and leave no filesystem-reserved character or
/// path-traversal surface.
pub fn validate_name(name: &str) -> Result<(), NameProblem> {
    if name.is_empty() {
        return Err(NameProblem::Empty);
    }
    if name.len() > TARGET_NAME_MAX_LEN {
        return Err(NameProblem::TooLong { len: name.len() });
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(NameProblem::InvalidChar);
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(NameProblem::EdgeDash);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_cli_rule_with_its_exact_reasons() {
        assert_eq!(validate_name("prod-1"), Ok(()));
        let long = "a".repeat(65);
        for (name, problem, reason) in [
            (
                "",
                NameProblem::Empty,
                "target name must not be empty".to_string(),
            ),
            (
                long.as_str(),
                NameProblem::TooLong { len: 65 },
                "target name must be ≤ 64 chars (got 65)".to_string(),
            ),
            (
                "a b",
                NameProblem::InvalidChar,
                "target name `a b` is invalid — allowed: alphanumeric + `-`".to_string(),
            ),
            (
                "-a",
                NameProblem::EdgeDash,
                "target name `-a` must not start or end with `-`".to_string(),
            ),
            (
                "a-",
                NameProblem::EdgeDash,
                "target name `a-` must not start or end with `-`".to_string(),
            ),
        ] {
            assert_eq!(validate_name(name), Err(problem), "{name:?}");
            assert_eq!(problem.reason(name), reason);
        }
        assert_eq!(NameProblem::TooLong { len: 65 }.as_str(), "too_long");
    }
}
