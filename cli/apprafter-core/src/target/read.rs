// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Target reads. Lockless (R3): the store's files are replaced atomically, so a read sees the
//! old file or the new one. They never run the legacy cwd state migration (CLI-only).

use crate::context::{Context, SecretString};
use crate::error::{CoreError, CoreResult};
use crate::target::PublicAddress;
use crate::{CancellationToken, TargetRef};

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
/// a 404 is [`CoreError::ServerMissing`]; an API status passes through as
/// `CliError::Hetzner` (its `status` projects); anything else (transport, timeout, parse) is
/// [`CoreError::ProviderRequestFailed`].
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
        Err(e @ cli_core::CliError::Hetzner { .. }) => Err(CoreError::Cli(e)),
        Err(e) => Err(CoreError::ProviderRequestFailed {
            provider: "hetzner-cloud".into(),
            endpoint,
            cause: Box::new(e.into()),
        }),
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
    fn an_api_status_passes_through_and_a_transport_error_is_wrapped() {
        let mut s = mockito::Server::new();
        s.mock("GET", "/v1/servers/42")
            .with_status(401)
            .with_body(r#"{"error":{"code":"unauthorized","message":"x"}}"#)
            .create();
        let (_d, ctx) = store(&s.url(), Some("tok"), true);
        let t = TargetRef::named(&ctx, "prod").unwrap();
        let e = public_address(&ctx, &t, &CancellationToken::new()).unwrap_err();
        assert_eq!(UiError::from(&e).fields["status"], serde_json::json!(401));
        let (_d, ctx) = store("http://127.0.0.1:1", Some("tok"), true);
        let t = TargetRef::named(&ctx, "prod").unwrap();
        assert!(
            matches!(public_address(&ctx, &t, &CancellationToken::new()),
            Err(CoreError::ProviderRequestFailed { ref endpoint, .. }) if endpoint == "GET /v1/servers/42")
        );
    }

    #[test]
    fn the_token_is_the_cli_override_else_the_stored_one() {
        let (dir, ctx) = store("http://unused", None, true);
        let t = TargetRef::named(&ctx, "prod").unwrap();
        assert!(
            matches!(hetzner_token(&ctx, &t), Err(CoreError::TokenNotStored { ref name }) if name == "prod")
        );
        let cli = Context::from_cli_env(
            &MapEnv::new()
                .with("APPRAFTER_CONFIG_DIR", dir.path().to_str().unwrap())
                .with("HCLOUD_TOKEN", "env-tok"),
        )
        .unwrap();
        assert_eq!(hetzner_token(&cli, &t).unwrap().expose(), "env-tok");
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
