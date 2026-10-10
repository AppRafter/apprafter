// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Which target an operation acts on (ADR 0067 §2).
//!
//! Every operation takes an explicit [`TargetRef`], and how it was found is
//! in the constructor's name: [`TargetRef::named`] for a name the caller
//! holds (`--target`, a desktop tab), [`TargetRef::active`] for the CLI's
//! active pointer. The desktop binds each tab with `named` and never calls
//! `active` for that (a desktop grep guard will enforce it).

use serde::Serialize;

use crate::context::Context;
use crate::error::{CoreError, CoreResult};

/// A target that exists in the store at the time it was resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TargetRef(String);

impl TargetRef {
    pub fn name(&self) -> &str {
        &self.0
    }

    /// The target called `name`, which must exist in the store.
    pub fn named(ctx: &Context, name: &str) -> CoreResult<TargetRef> {
        Self::existing(ctx, name.to_string())
    }

    /// The target the CLI's active pointer names, which must exist too: a
    /// pointer left dangling (its target removed by hand) is
    /// [`CoreError::TargetNotFound`] with the pointer's value as `name`; no
    /// pointer is [`CoreError::NoActiveTarget`].
    ///
    /// This is stricter than the CLI's `state_paths::resolve_state_paths`
    /// today, which checks that a `--target` name exists but takes the
    /// active pointer on trust. D.3 records a golden of the dangling-pointer
    /// case before the target family moves onto the core.
    pub fn active(ctx: &Context) -> CoreResult<TargetRef> {
        let name = cli_core::resolve_active_target_name(&ctx.store(), None)?
            .ok_or(CoreError::NoActiveTarget)?;
        Self::existing(ctx, name)
    }

    fn existing(ctx: &Context, name: String) -> CoreResult<TargetRef> {
        let available = cli_core::list_target_names(&ctx.store())?;
        if available.iter().any(|a| a == &name) {
            Ok(TargetRef(name))
        } else {
            Err(CoreError::TargetNotFound { name, available })
        }
    }
}

/// How an operation moved the CLI's active-target pointer as a side effect
/// (`target add` on an empty store, `rename` / `remove` of the active one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ActivePointerChange {
    pub from: Option<String>,
    pub to: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use cli_core::target::{GlobalConfig, Target, TargetConfig, TargetCredentials};
    use std::path::PathBuf;

    fn store_with(names: &[&str], active: Option<&str>) -> (tempfile::TempDir, Context) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::for_desktop(dir.path().to_path_buf(), "http://unused");
        let paths = ctx.store();
        for n in names {
            let t = Target {
                name: n.to_string(),
                config: TargetConfig {
                    provider: "hetzner-cloud".into(),
                    ..Default::default()
                },
                credentials: TargetCredentials::default(),
            };
            cli_core::save_target(&paths, &t).unwrap();
        }
        if let Some(a) = active {
            cli_core::save_global_config(
                &paths,
                &GlobalConfig {
                    active_target: a.to_string(),
                    version: cli_core::TARGET_STORE_VERSION,
                },
            )
            .unwrap();
        }
        (dir, ctx)
    }

    #[test]
    fn a_named_target_resolves_whatever_the_pointer_says() {
        let (_d, ctx) = store_with(&["prod", "staging"], Some("prod"));
        assert_eq!(TargetRef::named(&ctx, "staging").unwrap().name(), "staging");
        let (_d, ctx) = store_with(&["prod"], None);
        assert_eq!(TargetRef::named(&ctx, "prod").unwrap().name(), "prod");
    }

    #[test]
    fn an_unknown_name_lists_what_exists() {
        let (_d, ctx) = store_with(&["prod", "staging"], Some("prod"));
        match TargetRef::named(&ctx, "ghost") {
            Err(CoreError::TargetNotFound { name, available }) => {
                assert_eq!(name, "ghost");
                assert_eq!(available, vec!["prod".to_string(), "staging".to_string()]);
            }
            other => panic!("expected TargetNotFound, got {other:?}"),
        }
    }

    #[test]
    fn a_name_on_an_empty_store_is_not_found_with_nothing_available() {
        let (_d, ctx) = store_with(&[], None);
        assert!(matches!(
            TargetRef::named(&ctx, "prod"),
            Err(CoreError::TargetNotFound { ref name, ref available })
                if name == "prod" && available.is_empty()
        ));
    }

    #[test]
    fn active_resolves_to_the_pointer() {
        let (_d, ctx) = store_with(&["prod", "staging"], Some("staging"));
        assert_eq!(TargetRef::active(&ctx).unwrap().name(), "staging");
    }

    #[test]
    fn an_empty_store_has_no_active_target() {
        let (_d, ctx) = store_with(&[], None);
        assert!(matches!(
            TargetRef::active(&ctx),
            Err(CoreError::NoActiveTarget)
        ));
    }

    #[test]
    fn a_dangling_active_pointer_is_not_found() {
        let (_d, ctx) = store_with(&["prod"], Some("gone"));
        match TargetRef::active(&ctx) {
            Err(CoreError::TargetNotFound { name, available }) => {
                assert_eq!(name, "gone");
                assert_eq!(available, vec!["prod".to_string()]);
            }
            other => panic!("expected TargetNotFound, got {other:?}"),
        }
    }

    #[test]
    fn the_store_root_is_the_context_root() {
        let ctx = Context::for_desktop(PathBuf::from("/tmp/x"), "u");
        assert_eq!(ctx.store().root(), std::path::Path::new("/tmp/x"));
    }
}
