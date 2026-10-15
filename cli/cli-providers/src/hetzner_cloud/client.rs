// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Blocking HTTP wrapper around the Hetzner Cloud REST API.
//!
//! Pure I/O — no business logic. The provider in
//! `hetzner_cloud/provider.rs` calls these methods.

use cli_core::{CliError, Result};

use super::types::{ApiErrorEnvelope, ServerListResponse};

/// Default base URL for the public Hetzner Cloud API.
pub const DEFAULT_BASE_URL: &str = "https://api.hetzner.cloud";

/// A Hetzner Cloud API client: a base URL, a token, and the `ureq` agent every request goes
/// through. `Debug` never shows the token.
#[derive(Clone)]
pub struct HetznerCloudClient {
    pub base_url: String,
    pub token: String,
    agent: ureq::Agent,
}

impl std::fmt::Debug for HetznerCloudClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HetznerCloudClient")
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl HetznerCloudClient {
    /// A client on a default agent (no read timeout), as before: the CLI's own paths. Pass
    /// `DEFAULT_BASE_URL` for production; tests pass a `mockito::Server::url()`.
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self::with_agent(base_url, token, ureq::agent())
    }

    /// A client on `agent` — `apprafter_core::Context` hands one built with timeouts and a
    /// deadline-bound resolver.
    pub fn with_agent(
        base_url: impl Into<String>,
        token: impl Into<String>,
        agent: ureq::Agent,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            token: token.into(),
            agent,
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/v1{}", self.base_url.trim_end_matches('/'), path)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    pub fn list_servers(&self) -> Result<ServerListResponse> {
        let endpoint = self.endpoint("/servers");
        let resp = self
            .agent
            .get(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();

        match resp {
            Ok(r) => r
                .into_json::<ServerListResponse>()
                .map_err(|e| CliError::Other(format!("parse list_servers response: {e}"))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("GET {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    /// `GET /v1/locations` — list every Hetzner Cloud datacenter the
    /// account can target. Reading it requires nothing beyond a
    /// valid `Authorization` token (no quota, no resources touched,
    /// no rate-limit weight worth mentioning), which makes it the
    /// canonical pre-flight "is this token valid?" probe per
    /// `cli-dx-task.md` §11.
    pub fn list_locations(&self) -> Result<super::types::LocationListResponse> {
        let endpoint = self.endpoint("/locations");
        let resp = self
            .agent
            .get(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();

        match resp {
            Ok(r) => r
                .into_json::<super::types::LocationListResponse>()
                .map_err(|e| CliError::Other(format!("parse list_locations response: {e}"))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("GET {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    /// `GET /v1/servers/{id}`; `Ok(None)` when Hetzner answers 404.
    pub fn get_server(&self, id: u64) -> Result<Option<super::types::Server>> {
        let endpoint = self.endpoint(&format!("/servers/{id}"));
        let resp = self
            .agent
            .get(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();
        match resp {
            Ok(r) => r
                .into_json::<super::types::ServerResponse>()
                .map(|b| Some(b.server))
                .map_err(|e| CliError::Other(format!("parse get_server response: {e}"))),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(ureq::Error::Status(status, response)) => {
                Err(api_error("GET", &endpoint, status, response))
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    /// One page of `GET /v1/server_types` (50 per page) and the next page number, when it is
    /// greater than `page` — a non-advancing `next_page` (a misbehaving API returning the
    /// current or an earlier page) ends the listing, so a caller never loops forever.
    pub fn list_server_types_page(
        &self,
        page: u32,
    ) -> Result<(Vec<super::types::ServerType>, Option<u32>)> {
        let endpoint = self.endpoint("/server_types");
        let resp = self
            .agent
            .get(&endpoint)
            .query("page", &page.to_string())
            .query("per_page", "50")
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();

        let page_response = match resp {
            Ok(r) => r
                .into_json::<super::types::ServerTypeListResponse>()
                .map_err(|e| CliError::Other(format!("parse list_server_types response: {e}")))?,
            Err(ureq::Error::Status(status, response)) => {
                return Err(api_error("GET", &endpoint, status, response));
            }
            Err(ureq::Error::Transport(t)) => {
                return Err(transport_error(&endpoint, t));
            }
        };

        let next = page_response
            .meta
            .as_ref()
            .and_then(|m| m.pagination.next_page);
        Ok((page_response.server_types, next.filter(|n| *n > page)))
    }

    /// Every server type, page by page through [`Self::list_server_types_page`].
    pub fn list_server_types(&self) -> Result<super::types::ServerTypeListResponse> {
        let (mut all, mut page) = (Vec::new(), 1);
        loop {
            let (types, next) = self.list_server_types_page(page)?;
            all.extend(types);
            match next {
                Some(n) => page = n,
                None => break,
            }
        }
        Ok(super::types::ServerTypeListResponse {
            server_types: all,
            meta: None,
        })
    }

    pub fn list_ssh_keys(&self) -> Result<super::types::SshKeyListResponse> {
        let endpoint = self.endpoint("/ssh_keys");
        let resp = self
            .agent
            .get(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();

        match resp {
            Ok(r) => r
                .into_json::<super::types::SshKeyListResponse>()
                .map_err(|e| CliError::Other(format!("parse list_ssh_keys response: {e}"))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("GET {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    pub fn list_networks(&self) -> Result<super::types::NetworkListResponse> {
        let endpoint = self.endpoint("/networks");
        let resp = self
            .agent
            .get(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();

        match resp {
            Ok(r) => r
                .into_json::<super::types::NetworkListResponse>()
                .map_err(|e| CliError::Other(format!("parse list_networks response: {e}"))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("GET {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    pub fn create_network(
        &self,
        req: &super::types::NetworkCreateRequest,
    ) -> Result<super::types::NetworkCreateResponse> {
        let endpoint = self.endpoint("/networks");
        let resp = self
            .agent
            .post(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_json(req);

        match resp {
            Ok(r) => r
                .into_json::<super::types::NetworkCreateResponse>()
                .map_err(|e| CliError::Other(format!("parse create_network response: {e}"))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("POST {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    pub fn delete_network(&self, id: u64) -> Result<()> {
        let endpoint = self.endpoint(&format!("/networks/{id}"));
        delete_with_retry_on_transient_lock(&endpoint, || {
            self.agent
                .delete(&endpoint)
                .set("Authorization", &self.auth_header())
                .set("Accept", "application/json")
        })
    }

    pub fn list_firewalls(&self) -> Result<super::types::FirewallListResponse> {
        let endpoint = self.endpoint("/firewalls");
        let resp = self
            .agent
            .get(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();

        match resp {
            Ok(r) => r
                .into_json::<super::types::FirewallListResponse>()
                .map_err(|e| CliError::Other(format!("parse list_firewalls response: {e}"))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("GET {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    pub fn create_firewall(
        &self,
        req: &super::types::FirewallCreateRequest,
    ) -> Result<super::types::FirewallCreateResponse> {
        let endpoint = self.endpoint("/firewalls");
        let resp = self
            .agent
            .post(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_json(req);

        match resp {
            Ok(r) => r
                .into_json::<super::types::FirewallCreateResponse>()
                .map_err(|e| CliError::Other(format!("parse create_firewall response: {e}"))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("POST {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    /// Replace an existing firewall's entire rule set (1.83d — closes the
    /// create-only reconcile gap so Cloudflare-IP drift is picked up on
    /// re-apply).
    pub fn set_firewall_rules(&self, id: u64, rules: &[super::types::FirewallRule]) -> Result<()> {
        let endpoint = self.endpoint(&format!("/firewalls/{id}/actions/set_rules"));
        let req = super::types::SetFirewallRulesRequest {
            rules: rules.to_vec(),
        };
        let resp = self
            .agent
            .post(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_json(&req);

        match resp {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("POST {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    pub fn delete_firewall(&self, id: u64) -> Result<()> {
        let endpoint = self.endpoint(&format!("/firewalls/{id}"));
        delete_with_retry_on_transient_lock(&endpoint, || {
            self.agent
                .delete(&endpoint)
                .set("Authorization", &self.auth_header())
                .set("Accept", "application/json")
        })
    }

    pub fn delete_ssh_key(&self, id: u64) -> Result<()> {
        let endpoint = self.endpoint(&format!("/ssh_keys/{id}"));
        let resp = self
            .agent
            .delete(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();

        match resp {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(404, _)) => Ok(()),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("DELETE {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    pub fn create_ssh_key(
        &self,
        req: &super::types::SshKeyCreateRequest,
    ) -> Result<super::types::SshKeyCreateResponse> {
        let endpoint = self.endpoint("/ssh_keys");
        let resp = self
            .agent
            .post(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_json(req);

        match resp {
            Ok(r) => r
                .into_json::<super::types::SshKeyCreateResponse>()
                .map_err(|e| CliError::Other(format!("parse create_ssh_key response: {e}"))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("POST {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    pub fn delete_server(&self, id: u64) -> Result<()> {
        let endpoint = self.endpoint(&format!("/servers/{id}"));
        let resp = self
            .agent
            .delete(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();

        match resp {
            Ok(_) => Ok(()),
            // Idempotent delete: 404 means the server is already gone.
            Err(ureq::Error::Status(404, _)) => Ok(()),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("DELETE {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    pub fn create_server(
        &self,
        req: &super::types::ServerCreateRequest,
    ) -> Result<super::types::ServerCreateResponse> {
        let endpoint = self.endpoint("/servers");
        let resp = self
            .agent
            .post(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_json(req);

        match resp {
            Ok(r) => r
                .into_json::<super::types::ServerCreateResponse>()
                .map_err(|e| CliError::Other(format!("parse create_server response: {e}"))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("POST {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    pub fn list_floating_ips(&self) -> Result<super::types::FloatingIpListResponse> {
        let endpoint = self.endpoint("/floating_ips");
        let resp = self
            .agent
            .get(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();

        match resp {
            Ok(r) => r
                .into_json::<super::types::FloatingIpListResponse>()
                .map_err(|e| CliError::Other(format!("parse list_floating_ips response: {e}"))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("GET {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    pub fn create_floating_ip(
        &self,
        req: &super::types::FloatingIpCreateRequest,
    ) -> Result<super::types::FloatingIpCreateResponse> {
        let endpoint = self.endpoint("/floating_ips");
        let resp = self
            .agent
            .post(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_json(req);

        match resp {
            Ok(r) => r
                .into_json::<super::types::FloatingIpCreateResponse>()
                .map_err(|e| CliError::Other(format!("parse create_floating_ip response: {e}"))),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                Err(CliError::Hetzner {
                    endpoint: format!("POST {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    /// Unassign a Floating IP from any server it's currently
    /// attached to. Idempotent on these "already detached" signals:
    ///  - 404 — FIP gone entirely.
    ///  - 422 with `code=floating_ip_not_assigned` — Hetzner's
    ///    documented "specific" code (kept for forward-compat).
    ///  - 422 with `code=service_error` + message containing
    ///    "is not assigned" — Hetzner's current production
    ///    response when the IP is already detached. The
    ///    `service_error` code is intentionally generic upstream,
    ///    so the substring check is the only reliable signal.
    pub fn unassign_floating_ip(&self, id: u64) -> Result<()> {
        let endpoint = self.endpoint(&format!("/floating_ips/{id}/actions/unassign"));
        let resp = self
            .agent
            .post(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();

        match resp {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(404, _)) => Ok(()),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                let already_unassigned = envelope.error.code == "floating_ip_not_assigned"
                    || (status == 422 && envelope.error.message.contains("is not assigned"));
                if already_unassigned {
                    return Ok(());
                }
                Err(CliError::Hetzner {
                    endpoint: format!("POST {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                })
            }
            Err(ureq::Error::Transport(t)) => Err(transport_error(&endpoint, t)),
        }
    }

    /// Delete a Floating IP. Unassigns first so `DELETE` isn't
    /// rejected with `must_be_unassigned`. The `DELETE` itself
    /// uses [`delete_with_retry_on_transient_lock`] because
    /// Hetzner briefly locks the FIP (`423 locked`) while its
    /// async scheduler tears down the server→FIP association,
    /// which can persist a few seconds past
    /// `wait_for_server_gone` returning.
    pub fn delete_floating_ip(&self, id: u64) -> Result<()> {
        self.unassign_floating_ip(id)?;

        let endpoint = self.endpoint(&format!("/floating_ips/{id}"));
        let auth = self.auth_header();
        delete_with_retry_on_transient_lock(&endpoint, || {
            self.agent
                .delete(&endpoint)
                .set("Authorization", &auth)
                .set("Accept", "application/json")
        })
    }
}

/// Every request's transport failure, classified here once (WI-453): a request that got no
/// answer is [`CliError::ProviderApiUnreachable`], whichever request it was, so the core's
/// catalogue read and its token ping no longer tell the same refused connection apart; what
/// ureq refused to send, or an answer it could not read, stays the catch-all. Either way the
/// message names `endpoint` once.
fn transport_error(endpoint: &str, t: ureq::Transport) -> CliError {
    let failure = cli_core::TransportFailure {
        endpoint: endpoint.to_string(),
        detail: crate::transport::detail(&t),
    };
    if crate::transport::no_answer(t.kind()) {
        CliError::ProviderApiUnreachable {
            provider: "hetzner-cloud".to_string(),
            cause: Box::new(failure),
        }
    } else {
        CliError::Other(failure.to_string())
    }
}

/// The `CliError::Hetzner` for a non-2xx answer: the API's error envelope, or the raw body as
/// the message when it is not one.
fn api_error(method: &str, endpoint: &str, status: u16, response: ureq::Response) -> CliError {
    let body = response.into_string().unwrap_or_default();
    let envelope: ApiErrorEnvelope = serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
        error: super::types::ApiErrorDetails {
            code: "unknown".to_string(),
            message: body,
        },
    });
    CliError::Hetzner {
        endpoint: format!("{method} {endpoint}"),
        status,
        code: envelope.error.code,
        message: envelope.error.message,
    }
}

/// Retry a `DELETE` while Hetzner's async cleanup scheduler is
/// still holding on to the resource — covers two distinct
/// transient signals:
///
///  - `422 resource_in_use` from `delete_firewall` /
///    `delete_network` (firewall.applied_to and network.servers
///    still list the deleted server for ~1–15 s after
///    `DELETE /servers/{id}` returned).
///  - `423 locked` from `delete_floating_ip` (Hetzner locks the
///    FIP while it auto-unassigns from a freshly-deleted server;
///    can persist a few seconds past `wait_for_server_gone`).
///
/// 60 s deadline, exponential back-off (500 ms → 5 s cap). Any
/// other status / code is returned verbatim.
///
/// The closure returns a fresh `ureq::Request` each iteration
/// (since `Request::call` consumes self) and the helper does the
/// `.call()` internally — keeping the closure return type free of
/// `Result<_, ureq::Error>` so clippy's `result_large_err` lint
/// (active in rust 1.95+) doesn't fire on the 272-byte
/// `ureq::Error` variant.
///
/// Lives at module scope (rather than as a method on
/// `HetznerCloudClient`) so the borrow checker is happy with the
/// closure capturing `&self` only for the inner ureq call.
fn delete_with_retry_on_transient_lock<F>(endpoint: &str, mut build_request: F) -> Result<()>
where
    F: FnMut() -> ureq::Request,
{
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut delay = Duration::from_millis(500);

    loop {
        match build_request().call() {
            Ok(_) => return Ok(()),
            Err(ureq::Error::Status(404, _)) => return Ok(()),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let envelope: ApiErrorEnvelope =
                    serde_json::from_str(&body).unwrap_or(ApiErrorEnvelope {
                        error: super::types::ApiErrorDetails {
                            code: "unknown".to_string(),
                            message: body,
                        },
                    });
                let retryable = envelope.error.code == "resource_in_use" || status == 423;
                if retryable && Instant::now() < deadline {
                    sleep(delay);
                    delay = (delay * 2).min(Duration::from_secs(5));
                    continue;
                }
                return Err(CliError::Hetzner {
                    endpoint: format!("DELETE {endpoint}"),
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                });
            }
            Err(ureq::Error::Transport(t)) => return Err(transport_error(endpoint, t)),
        }
    }
}
