// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Where a [`Context`](crate::Context) builder reads its inputs.
//!
//! The core never calls `std::env` itself (ADR 0067 §2). The CLI passes an
//! implementation backed by the process environment; tests pass a
//! [`MapEnv`].

use std::collections::BTreeMap;
use std::ffi::OsString;

/// A read-only view of environment variables.
pub trait EnvSource {
    /// The value of `key`, or `None` when unset. A value that is not valid
    /// Unicode comes back as `None` too, like `std::env::var(..).ok()`. An
    /// empty value comes back as `Some("")`; each consumer decides whether
    /// empty means unset, exactly as the CLI did before the core existed.
    fn var(&self, key: &str) -> Option<String>;

    /// The value of `key` as the OS holds it, for a name whose value need not be Unicode
    /// (`PATH`). By default it is [`EnvSource::var`]'s; a client backed by the process
    /// environment overrides it so a non-Unicode value survives.
    fn var_os(&self, key: &str) -> Option<OsString> {
        self.var(key).map(OsString::from)
    }
}

/// An in-memory [`EnvSource`] for tests and for callers with no environment.
#[derive(Debug, Clone, Default)]
pub struct MapEnv(BTreeMap<String, String>);

impl MapEnv {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: the same map with `key` set to `value`.
    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.0.insert(key.to_string(), value.to_string());
        self
    }
}

impl EnvSource for MapEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_env_returns_set_values_and_none_for_unset() {
        let env = MapEnv::new().with("A", "1").with("EMPTY", "");
        assert_eq!(env.var("A").as_deref(), Some("1"));
        assert_eq!(env.var("EMPTY").as_deref(), Some(""));
        assert_eq!(env.var("MISSING"), None);
    }

    #[test]
    fn var_os_defaults_to_the_string_value() {
        assert_eq!(
            MapEnv::new().with("P", "/x").var_os("P"),
            Some(std::ffi::OsString::from("/x"))
        );
        assert_eq!(MapEnv::new().var_os("P"), None);
    }
}
