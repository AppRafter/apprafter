// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Which target an operation acts on (ADR 0067 §2).
//!
//! Every operation takes an explicit [`TargetRef`]. The CLI resolves it from
//! `--target` or its active pointer; the desktop passes the tab's target and
//! never reads the pointer for that purpose.

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

    /// `explicit` when given (it must exist), else the store's active
    /// target.
    pub fn resolve(ctx: &Context, explicit: Option<&str>) -> CoreResult<TargetRef> {
        let store = ctx.store();
        let available = cli_core::list_target_names(&store)?;
        let name = match explicit {
            Some(n) => n.to_string(),
            None => cli_core::resolve_active_target_name(&store, None)?
                .ok_or(CoreError::NoActiveTarget)?,
        };
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
    fn an_explicit_existing_name_resolves() {
        let (_d, ctx) = store_with(&["prod", "staging"], Some("prod"));
        assert_eq!(
            TargetRef::resolve(&ctx, Some("staging")).unwrap().name(),
            "staging"
        );
    }

    #[test]
    fn no_name_resolves_to_the_active_target() {
        let (_d, ctx) = store_with(&["prod", "staging"], Some("prod"));
        assert_eq!(TargetRef::resolve(&ctx, None).unwrap().name(), "prod");
    }

    #[test]
    fn an_unknown_name_lists_what_exists() {
        let (_d, ctx) = store_with(&["prod", "staging"], Some("prod"));
        match TargetRef::resolve(&ctx, Some("ghost")) {
            Err(CoreError::TargetNotFound { name, available }) => {
                assert_eq!(name, "ghost");
                assert_eq!(available, vec!["prod".to_string(), "staging".to_string()]);
            }
            other => panic!("expected TargetNotFound, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_store_has_no_active_target() {
        let (_d, ctx) = store_with(&[], None);
        assert!(matches!(
            TargetRef::resolve(&ctx, None),
            Err(CoreError::NoActiveTarget)
        ));
    }

    #[test]
    fn a_dangling_active_pointer_is_not_found() {
        let (_d, ctx) = store_with(&["prod"], Some("gone"));
        assert!(matches!(
            TargetRef::resolve(&ctx, None),
            Err(CoreError::TargetNotFound { ref name, .. }) if name == "gone"
        ));
    }

    #[test]
    fn the_store_root_is_the_context_root() {
        let ctx = Context::for_desktop(PathBuf::from("/tmp/x"), "u");
        assert_eq!(ctx.store().root(), std::path::Path::new("/tmp/x"));
    }
}
