// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Target reads. Lockless (R3): the store's files are replaced atomically, so a read sees the
//! old file or the new one. They never run the legacy cwd state migration (CLI-only).

use crate::context::{Context, SecretString};
use crate::error::{CoreError, CoreResult, UiError};
use crate::ssh::inspect_key;
use crate::target::{
    cli_default, provisioned, CliDefaultPointer, ProvisionedState, PublicAddress, TargetListReport,
    TargetReport, TargetSummary, TokenPresence, UnreadableTarget,
};
use crate::{CancellationToken, TargetRef};

/// Every target, by its `config.yaml` only (a list never reads a secret). A target whose config
/// cannot be read is an `unreadable` row, never dropped. Lockless (R3).
pub fn list(ctx: &Context) -> CoreResult<TargetListReport> {
    let store = ctx.store();
    let names = cli_core::list_target_names(&store)?;
    let pointer = cli_default(ctx)?;
    let (mut targets, mut unreadable) = (Vec::new(), Vec::new());
    for name in &names {
        match cli_core::target::load_target_config(&store, name) {
            Ok(c) => targets.push(TargetSummary {
                name: name.clone(),
                provider: c.provider,
                tier_level: tier_level(c.default_tier.as_deref()),
                region: c.region,
                server_type: c.server_type,
                default_tier: c.default_tier,
                is_cli_default: pointer.as_deref() == Some(name.as_str()),
            }),
            Err(e) => unreadable.push(UnreadableTarget {
                name: name.clone(),
                error: UiError::from(&CoreError::from(e)),
            }),
        }
    }
    let cli_default = match pointer {
        None => CliDefaultPointer::Unset,
        Some(n) if names.contains(&n) => CliDefaultPointer::Set { name: n },
        Some(n) => CliDefaultPointer::Missing { name: n },
    };
    Ok(TargetListReport {
        targets,
        unreadable,
        cli_default,
    })
}

/// 1..=4 when `tier` names one of the four tiers; `None` for free text.
fn tier_level(tier: Option<&str>) -> Option<u8> {
    tier.and_then(|t| t.parse::<cli_core::Tier>().ok())
        .map(cli_core::Tier::level)
}

/// One target in full, never its token. `provisioned` comes from `state/<name>` only; a corrupt
/// state is `Unreadable`, not an error, so the rest still shows. Lockless (R3).
pub fn show(ctx: &Context, target: &TargetRef) -> CoreResult<TargetReport> {
    let store = ctx.store();
    let name = target.name();
    let t = cli_core::load_target(&store, name)?;
    let token = t.credentials.hetzner_token.as_deref();
    Ok(TargetReport {
        name: name.to_string(),
        is_cli_default: cli_default(ctx)?.as_deref() == Some(name),
        provider: t.config.provider,
        tier_level: tier_level(t.config.default_tier.as_deref()),
        region: t.config.region,
        server_type: t.config.server_type,
        default_tier: t.config.default_tier,
        cluster_name: t.config.cluster_name,
        ssh_key: t
            .config
            .ssh_key_path
            .as_deref()
            .map(|p| inspect_key(ctx, p))
            .transpose()?,
        token: TokenPresence {
            set: token.is_some(),
            chars: token.map(|t| t.len() as u32),
        },
        config_file: store.target_config_file(name).display().to_string(),
        credentials_file: store.target_credentials_file(name).display().to_string(),
        provisioned: match provisioned(ctx, target) {
            Ok(None) => ProvisionedState::NotProvisioned,
            Ok(Some(server)) => ProvisionedState::Provisioned { server },
            Err(e) => ProvisionedState::Unreadable {
                error: UiError::from(&e),
            },
        },
    })
}

/// The Hetzner token for `target`: the CLI's `HCLOUD_TOKEN` override first, else the stored
/// one — today's `resolve_hetzner_token` order (R4). The desktop has no override.
pub fn hetzner_token(ctx: &Context, target: &TargetRef) -> CoreResult<SecretString> {
    if let Some(token) = &ctx.overrides().hetzner_token {
        return Ok(token.clone());
    }
    cli_core::load_target(&ctx.store(), target.name())?
        .credentials
        .hetzner_token
        .map(SecretString::new)
        .ok_or_else(|| CoreError::TokenNotStored {
            name: target.name().to_string(),
        })
}

/// The node's public IPv4 and IPv6 (`<prefix>::1`) from `GET /v1/servers/{id}` of the server
/// `state.json` records — one request, so an account with more than one page of servers is
/// answered right. No recorded server is [`CoreError::NotProvisioned`] (nothing is asked);
/// a 404 is [`CoreError::ServerMissing`]; any other failure is classified as every provider
/// read's is (`provider::read_error`): an API status passes through as `CliError::Hetzner` (its
/// `status` projects), no answer as `CliError::ProviderApiUnreachable`, an answer that does not
/// parse is [`CoreError::ProviderRequestFailed`].
pub fn public_address(
    ctx: &Context,
    target: &TargetRef,
    cancel: &CancellationToken,
) -> CoreResult<PublicAddress> {
    let paths = cli_state::StatePaths::for_active_target(&ctx.store(), target.name());
    let state = cli_state::State::load_or_default(&paths)?;
    let Some(server) = state.hetzner_cloud else {
        return Err(CoreError::NotProvisioned {
            name: target.name().to_string(),
        });
    };
    let token = hetzner_token(ctx, target)?;
    cancel.check()?;
    let endpoint = format!("GET /v1/servers/{}", server.server_id);
    match ctx.hetzner_client(&token).get_server(server.server_id) {
        Ok(Some(found)) => {
            let (ipv4, ipv6) =
                cli_providers::extract_public_ips(std::slice::from_ref(&found), found.id);
            Ok(PublicAddress {
                server_id: found.id,
                ipv4,
                ipv6,
            })
        }
        Ok(None) => Err(CoreError::ServerMissing {
            name: target.name().to_string(),
            server_id: server.server_id,
        }),
        Err(e) => Err(crate::provider::read_error(e, &endpoint)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;
    use crate::error::UiError;

    fn store(url: &str, token: Option<&str>, provisioned: bool) -> (tempfile::TempDir, Context) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::for_desktop(dir.path().to_path_buf(), url);
        cli_core::save_target(
            &ctx.store(),
            &cli_core::Target {
                name: "prod".into(),
                config: cli_core::TargetConfig {
                    provider: "hetzner-cloud".into(),
                    ..Default::default()
                },
                credentials: cli_core::TargetCredentials {
                    hetzner_token: token.map(str::to_string),
                },
            },
        )
        .unwrap();
        if provisioned {
            let state = dir.path().join("state/prod/.apprafter");
            std::fs::create_dir_all(&state).unwrap();
            std::fs::write(
                state.join("state.json"),
                r#"{"hetzner_cloud":{"server_id":42,"server_name":"prod-node"}}"#,
            )
            .unwrap();
        }
        (dir, ctx)
    }

    const SERVER: &str = r#"{"server":{"id":42,"name":"prod-node","status":"running","labels":{},
  "public_net":{"ipv4":{"ip":"203.0.113.10"},"ipv6":{"ip":"2001:db8:1::/64"}}}}"#;

    #[test]
    fn the_address_is_read_by_server_id() {
        let mut s = mockito::Server::new();
        let m = s
            .mock("GET", "/v1/servers/42")
            .match_header("authorization", "Bearer tok")
            .with_status(200)
            .with_body(SERVER)
            .create();
        let (_d, ctx) = store(&s.url(), Some("tok"), true);
        let target = TargetRef::named(&ctx, "prod").unwrap();
        let addr = public_address(&ctx, &target, &CancellationToken::new()).unwrap();
        assert_eq!(
            addr,
            PublicAddress {
                server_id: 42,
                ipv4: Some("203.0.113.10".into()),
                ipv6: Some("2001:db8:1::1".into())
            }
        );
        m.assert();
    }

    #[test]
    fn no_recorded_server_is_not_provisioned_and_asks_nothing() {
        let mut s = mockito::Server::new();
        let m = s.mock("GET", mockito::Matcher::Any).expect(0).create();
        let (_d, ctx) = store(&s.url(), Some("tok"), false);
        let target = TargetRef::named(&ctx, "prod").unwrap();
        assert!(
            matches!(public_address(&ctx, &target, &CancellationToken::new()), Err(CoreError::NotProvisioned { ref name }) if name == "prod")
        );
        m.assert();
    }

    #[test]
    fn a_server_gone_at_the_provider_is_server_missing() {
        let mut s = mockito::Server::new();
        s.mock("GET", "/v1/servers/42")
            .with_status(404)
            .with_body(r#"{"error":{"code":"not_found","message":"x"}}"#)
            .create();
        let (_d, ctx) = store(&s.url(), Some("tok"), true);
        let target = TargetRef::named(&ctx, "prod").unwrap();
        assert!(matches!(
            public_address(&ctx, &target, &CancellationToken::new()),
            Err(CoreError::ServerMissing { server_id: 42, .. })
        ));
    }

    #[test]
    fn an_api_status_passes_through_a_dead_api_is_unreachable_and_a_bad_answer_is_wrapped() {
        let mut s = mockito::Server::new();
        s.mock("GET", "/v1/servers/42")
            .with_status(401)
            .with_body(r#"{"error":{"code":"unauthorized","message":"x"}}"#)
            .create();
        let (_d, ctx) = store(&s.url(), Some("tok"), true);
        let t = TargetRef::named(&ctx, "prod").unwrap();
        let e = public_address(&ctx, &t, &CancellationToken::new()).unwrap_err();
        assert_eq!(UiError::from(&e).fields["status"], serde_json::json!(401));
        // WI-453: the code every other core path gives a dead API.
        let (_d, ctx) = store("http://127.0.0.1:1", Some("tok"), true);
        let t = TargetRef::named(&ctx, "prod").unwrap();
        let e = public_address(&ctx, &t, &CancellationToken::new()).unwrap_err();
        assert_eq!(
            UiError::from(&e).code.as_deref(),
            Some("apprafter::target::provider_unreachable"),
            "{e:?}"
        );
        // An answer that does not parse names the endpoint.
        let mut s = mockito::Server::new();
        s.mock("GET", "/v1/servers/42")
            .with_status(200)
            .with_body(r#"{"nope":1}"#)
            .create();
        let (_d, ctx) = store(&s.url(), Some("tok"), true);
        let t = TargetRef::named(&ctx, "prod").unwrap();
        assert!(
            matches!(public_address(&ctx, &t, &CancellationToken::new()),
            Err(CoreError::ProviderRequestFailed { ref endpoint, .. }) if endpoint == "GET /v1/servers/42")
        );
    }

    #[test]
    fn the_token_is_the_cli_override_else_the_stored_one() {
        let (_none, ctx) = store("http://unused", None, true);
        let t = TargetRef::named(&ctx, "prod").unwrap();
        assert!(
            matches!(hetzner_token(&ctx, &t), Err(CoreError::TokenNotStored { ref name }) if name == "prod")
        );
        // Both present: the override still wins (R4), so a stored-first order fails here.
        let (dir, ctx) = store("http://unused", Some("stored-tok"), true);
        let t = TargetRef::named(&ctx, "prod").unwrap();
        assert_eq!(hetzner_token(&ctx, &t).unwrap().expose(), "stored-tok");
        let cli = |env: MapEnv| {
            Context::from_cli_env(&env.with("APPRAFTER_CONFIG_DIR", dir.path().to_str().unwrap()))
                .unwrap()
        };
        let with_override = cli(MapEnv::new().with("HCLOUD_TOKEN", "env-tok"));
        assert_eq!(
            hetzner_token(&with_override, &t).unwrap().expose(),
            "env-tok"
        );
        assert_eq!(
            hetzner_token(&cli(MapEnv::new()), &t).unwrap().expose(),
            "stored-tok"
        );
    }

    #[test]
    fn a_cancelled_read_asks_nothing() {
        let mut s = mockito::Server::new();
        let m = s.mock("GET", mockito::Matcher::Any).expect(0).create();
        let (_d, ctx) = store(&s.url(), Some("tok"), true);
        let t = TargetRef::named(&ctx, "prod").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            public_address(&ctx, &t, &cancel),
            Err(CoreError::Cancelled)
        ));
        m.assert();
    }
}

#[cfg(test)]
mod list_show_tests {
    use super::super::testkit::*;
    use super::*;
    use crate::target::{CliDefaultPointer, ProvisionedState, TokenPresence};

    #[test]
    fn list_reports_unreadable_targets_and_never_reads_credentials() {
        let (_d, ctx) = store(&["a", "b", "c"], Some("b"));
        std::fs::write(ctx.store().target_config_file("a"), "provider: [").unwrap(); // corrupt config
        std::fs::write(ctx.store().target_credentials_file("c"), "hetzner_token: [").unwrap(); // corrupt credentials
        edit(&ctx, "b", |t| t.config.default_tier = Some("team".into()));
        let r = list(&ctx).unwrap();
        assert_eq!(
            r.targets
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "c"]
        );
        assert_eq!(r.unreadable.len(), 1);
        assert_eq!(r.unreadable[0].name, "a");
        assert_eq!(
            r.unreadable[0].error.code.as_deref(),
            Some("apprafter::target::invalid_config")
        );
        let b = &r.targets[0];
        assert!(
            b.is_cli_default
                && b.tier_level == Some(2)
                && b.default_tier.as_deref() == Some("team")
        );
        assert_eq!(r.cli_default, CliDefaultPointer::Set { name: "b".into() });
    }

    #[test]
    fn list_tells_an_unset_pointer_from_a_dangling_one() {
        let (_d, ctx) = store(&["a"], None);
        assert_eq!(list(&ctx).unwrap().cli_default, CliDefaultPointer::Unset);
        let (_d, ctx) = store(&["a"], Some("gone"));
        let r = list(&ctx).unwrap();
        assert_eq!(
            r.cli_default,
            CliDefaultPointer::Missing {
                name: "gone".into()
            }
        );
        assert!(!r.targets[0].is_cli_default);
    }

    #[test]
    fn a_free_text_tier_has_no_level() {
        let (_d, ctx) = store(&["a"], None);
        edit(&ctx, "a", |t| {
            t.config.default_tier = Some("bespoke".into())
        });
        let a = &list(&ctx).unwrap().targets[0];
        assert_eq!(
            (a.default_tier.as_deref(), a.tier_level),
            (Some("bespoke"), None)
        );
    }

    #[test]
    fn show_reports_the_token_by_length_only_and_the_provisioned_server() {
        let (dir, ctx) = store(&["prod"], Some("prod"));
        // joined, so the display below uses the OS separator
        let key = dir.path().join("home").join(".ssh").join("id_ed25519.pub");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, "ssh-ed25519 AAAA me@laptop\n").unwrap();
        edit(&ctx, "prod", |t| t.config.ssh_key_path = Some(key.clone()));
        seed_server(&ctx, "prod", 7, "platform-1", None);
        let r = show(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        assert!(r.is_cli_default);
        assert_eq!(
            r.token,
            TokenPresence {
                set: true,
                chars: Some(64)
            }
        );
        assert!(!serde_json::to_string(&r).unwrap().contains(TOKEN_A));
        let ssh = r.ssh_key.unwrap();
        // `~/.ssh\id_ed25519.pub` on CI's windows-latest leg: the expected text is built the way
        // `abbreviate_home` builds it
        let shown = format!(
            "~/{}",
            std::path::Path::new(".ssh")
                .join("id_ed25519.pub")
                .display()
        );
        assert_eq!(
            (ssh.display.as_str(), ssh.exists, ssh.algo.as_deref()),
            (shown.as_str(), true, Some("ssh-ed25519"))
        );
        assert!(
            matches!(r.provisioned, ProvisionedState::Provisioned { ref server } if server.server_id == 7)
        );
        assert_eq!(
            r.config_file,
            ctx.store().target_config_file("prod").display().to_string()
        );
    }

    #[test]
    fn show_of_a_target_with_corrupt_state_still_answers() {
        let (_d, ctx) = store(&["prod"], Some("prod"));
        seed_state_raw(&ctx, "prod", "{");
        let r = show(&ctx, &TargetRef::named(&ctx, "prod").unwrap()).unwrap();
        assert!(
            matches!(r.provisioned, ProvisionedState::Unreadable { ref error } if error.code.as_deref() == Some("apprafter::state::corrupt"))
        );
    }
}
