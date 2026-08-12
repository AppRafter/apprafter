// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure resolution helpers for provisioning-axis precedence (2.16h).
//!
//! Each provisioning axis (server type, region, …) has a fixed priority
//! order:  explicit CLI flag  >  manifest value  >  recorded state fact  >
//! saved target preference  >  ambient env var.  The `resolve_precedence`
//! function encodes that table as a single `Option::or` chain so callers
//! don't re-implement it ad-hoc.

/// First-Some precedence for a provisioning axis. The order encodes the
/// resolution table (2.16h): explicit per-invocation flag beats a committed
/// manifest, which beats the recorded state fact, which beats the saved target
/// preference, which beats an ambient env var. Returns None when every rung is
/// empty (the caller decides the default: `nbg1` for region, an error for type).
pub fn resolve_precedence(
    flag: Option<&str>,
    manifest: Option<&str>,
    state: Option<&str>,
    target: Option<&str>,
    env: Option<&str>,
) -> Option<String> {
    flag.or(manifest)
        .or(state)
        .or(target)
        .or(env)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::resolve_precedence;

    #[test]
    fn resolve_precedence_order_flag_manifest_state_target_env() {
        assert_eq!(
            resolve_precedence(Some("f"), Some("m"), Some("s"), Some("t"), Some("e")).as_deref(),
            Some("f")
        );
        assert_eq!(
            resolve_precedence(None, Some("m"), Some("s"), Some("t"), Some("e")).as_deref(),
            Some("m")
        );
        assert_eq!(
            resolve_precedence(None, None, Some("s"), Some("t"), Some("e")).as_deref(),
            Some("s")
        );
        assert_eq!(
            resolve_precedence(None, None, None, Some("t"), Some("e")).as_deref(),
            Some("t")
        );
        assert_eq!(
            resolve_precedence(None, None, None, None, Some("e")).as_deref(),
            Some("e")
        );
        assert_eq!(resolve_precedence(None, None, None, None, None), None);
    }
}
