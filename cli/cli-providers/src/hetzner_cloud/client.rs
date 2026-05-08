// SPDX-License-Identifier: FSL-1.1-MIT
//! Blocking HTTP wrapper around the Hetzner Cloud REST API.
//!
//! Pure I/O — no business logic. The provider in
//! `hetzner_cloud/provider.rs` calls these methods.

use cli_core::{CliError, Result};

use super::types::{ApiErrorEnvelope, ServerListResponse};

/// Default base URL for the public Hetzner Cloud API.
pub const DEFAULT_BASE_URL: &str = "https://api.hetzner.cloud";

#[derive(Debug, Clone)]
pub struct HetznerCloudClient {
    pub base_url: String,
    pub token: String,
}

impl HetznerCloudClient {
    /// Construct a client. Pass `DEFAULT_BASE_URL` for production;
    /// tests pass a `mockito::Server::url()`.
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            token: token.into(),
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
        let resp = ureq::get(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn list_server_types(&self) -> Result<super::types::ServerTypeListResponse> {
        // Pagination defaults to 25 results; the full list is typically small
        // enough that a single page suffices for pre-flight validation.
        let endpoint = self.endpoint("/server_types");
        let resp = ureq::get(&endpoint)
            .set("Authorization", &self.auth_header())
            .set("Accept", "application/json")
            .call();

        match resp {
            Ok(r) => r
                .into_json::<super::types::ServerTypeListResponse>()
                .map_err(|e| CliError::Other(format!("parse list_server_types response: {e}"))),
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn list_ssh_keys(&self) -> Result<super::types::SshKeyListResponse> {
        let endpoint = self.endpoint("/ssh_keys");
        let resp = ureq::get(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn list_networks(&self) -> Result<super::types::NetworkListResponse> {
        let endpoint = self.endpoint("/networks");
        let resp = ureq::get(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn create_network(
        &self,
        req: &super::types::NetworkCreateRequest,
    ) -> Result<super::types::NetworkCreateResponse> {
        let endpoint = self.endpoint("/networks");
        let resp = ureq::post(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn delete_network(&self, id: u64) -> Result<()> {
        let endpoint = self.endpoint(&format!("/networks/{id}"));
        let resp = ureq::delete(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn list_firewalls(&self) -> Result<super::types::FirewallListResponse> {
        let endpoint = self.endpoint("/firewalls");
        let resp = ureq::get(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn create_firewall(
        &self,
        req: &super::types::FirewallCreateRequest,
    ) -> Result<super::types::FirewallCreateResponse> {
        let endpoint = self.endpoint("/firewalls");
        let resp = ureq::post(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn delete_firewall(&self, id: u64) -> Result<()> {
        let endpoint = self.endpoint(&format!("/firewalls/{id}"));
        let resp = ureq::delete(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn delete_ssh_key(&self, id: u64) -> Result<()> {
        let endpoint = self.endpoint(&format!("/ssh_keys/{id}"));
        let resp = ureq::delete(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn create_ssh_key(
        &self,
        req: &super::types::SshKeyCreateRequest,
    ) -> Result<super::types::SshKeyCreateResponse> {
        let endpoint = self.endpoint("/ssh_keys");
        let resp = ureq::post(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn delete_server(&self, id: u64) -> Result<()> {
        let endpoint = self.endpoint(&format!("/servers/{id}"));
        let resp = ureq::delete(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn create_server(
        &self,
        req: &super::types::ServerCreateRequest,
    ) -> Result<super::types::ServerCreateResponse> {
        let endpoint = self.endpoint("/servers");
        let resp = ureq::post(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn list_floating_ips(&self) -> Result<super::types::FloatingIpListResponse> {
        let endpoint = self.endpoint("/floating_ips");
        let resp = ureq::get(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn create_floating_ip(
        &self,
        req: &super::types::FloatingIpCreateRequest,
    ) -> Result<super::types::FloatingIpCreateResponse> {
        let endpoint = self.endpoint("/floating_ips");
        let resp = ureq::post(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }

    pub fn delete_floating_ip(&self, id: u64) -> Result<()> {
        let endpoint = self.endpoint(&format!("/floating_ips/{id}"));
        let resp = ureq::delete(&endpoint)
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
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error talking to {endpoint}: {t}"
            ))),
        }
    }
}
