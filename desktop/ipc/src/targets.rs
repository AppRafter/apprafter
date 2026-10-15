// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the target commands send that the core's own types do not cover: the token draft and
//! the shapes that name it (design decision 6). The Hetzner token crosses IPC once, as
//! `op_start_verify_token`'s own `token` parameter, and waits in Rust as a draft; nothing here
//! can hold it.

use serde::{Deserialize, Serialize};

/// Names a verified token waiting in Rust (ten minutes; dropped on a lock). Below 2^53, a bare
/// number both ways, like `OpId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DraftId(pub u64);

/// `op_start_verify_token`'s result: the provider accepted the token, which waits as `draft_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TokenVerified {
    pub draft_id: DraftId,
    pub elapsed_ms: u64,
}

/// `op_plan_target_add`'s argument: what the add wizard collected, the token by its draft.
/// `deny_unknown_fields`: a page that sends a `token` here is refused, not ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetAddArgs {
    pub name: String,
    pub provider: String,
    pub draft_id: DraftId,
    pub ssh_key: Option<String>,
    pub region: Option<String>,
    pub tier: Option<String>,
    pub server_type: Option<String>,
}

/// Whose token `op_start_machine_catalogue` reads with: a draft's (the add wizard) or a saved
/// target's (Machine › Change).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogueSourceArg {
    Draft {
        #[serde(rename = "draftId")]
        draft_id: DraftId,
    },
    Target {
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_draft_id_is_a_bare_number_both_ways() {
        assert_eq!(serde_json::to_string(&DraftId(3)).unwrap(), "3");
        assert_eq!(serde_json::from_str::<DraftId>("3").unwrap(), DraftId(3));
    }

    #[test]
    fn token_verified_names_the_draft_never_the_token() {
        let wire = serde_json::to_value(TokenVerified {
            draft_id: DraftId(3),
            elapsed_ms: 182,
        })
        .unwrap();
        assert_eq!(wire, json!({ "draftId": 3, "elapsedMs": 182 }));
    }

    #[test]
    fn the_add_arguments_take_a_draft_and_refuse_a_token_field() {
        let args: TargetAddArgs = serde_json::from_value(json!({
            "name": "prod", "provider": "hetzner-cloud", "draftId": 3,
            "sshKey": null, "region": "nbg1", "tier": "solo", "serverType": "cx22",
        }))
        .unwrap();
        assert_eq!(args.draft_id, DraftId(3));
        assert_eq!(args.server_type.as_deref(), Some("cx22"));
        let smuggled = json!({ "name": "prod", "provider": "hetzner-cloud", "draftId": 3,
            "sshKey": null, "region": null, "tier": null, "serverType": null, "token": "x" });
        assert!(
            serde_json::from_value::<TargetAddArgs>(smuggled).is_err(),
            "no token field, ever"
        );
    }

    #[test]
    fn a_catalogue_source_is_a_draft_or_a_target() {
        assert_eq!(
            serde_json::from_value::<CatalogueSourceArg>(json!({ "kind": "draft", "draftId": 3 }))
                .unwrap(),
            CatalogueSourceArg::Draft {
                draft_id: DraftId(3)
            }
        );
        assert_eq!(
            serde_json::from_value::<CatalogueSourceArg>(
                json!({ "kind": "target", "name": "prod" })
            )
            .unwrap(),
            CatalogueSourceArg::Target {
                name: "prod".into()
            }
        );
    }

    #[cfg(feature = "ts")]
    #[test]
    fn the_typescript_names_the_draft_in_camel_case() {
        use ts_rs::TS;
        let cfg = ts_rs::Config::new().with_large_int("number");
        let add = TargetAddArgs::decl(&cfg);
        assert!(
            add.contains("draftId: DraftId") && !add.contains("token"),
            "{add}"
        );
        let source = CatalogueSourceArg::decl(&cfg);
        assert!(
            source.contains("\"kind\": \"draft\"") && source.contains("draftId"),
            "{source}"
        );
    }
}
