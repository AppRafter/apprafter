// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Resolve an OCI image tag to its current registry digest
//! (`repo@sha256:…`) via a manifest `HEAD` reading
//! `Docker-Content-Digest`. Pure parsers + an injected HTTP seam
//! (`RegistryHttp`) so the bearer-token flow is unit-testable
//! without a live registry — the I/O lives in the controller.

// The parsers + error type are wired into the controller's resolve
// flow in the following 2.4h-a/2.4h-d tasks; the whole module reads as
// dead code until those call sites land.
#![allow(dead_code)]

use oci_distribution::Reference;
use std::collections::HashMap;
use thiserror::Error;

/// Registry host + repository path + reference (tag or digest)
/// split out of an image string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub host: String,
    pub repository: String,
    pub reference: String,
    pub is_digest: bool,
}

#[derive(Debug, Error)]
pub enum OciResolveError {
    #[error("invalid image reference {0:?}")]
    InvalidReference(String),
    #[error("registry HTTP error: {0}")]
    Http(String),
    #[error("registry returned status {status} for {url}")]
    Status { status: u16, url: String },
    #[error("registry response missing Docker-Content-Digest")]
    NoDigestHeader,
    #[error("could not authenticate to registry: {0}")]
    Auth(String),
}

/// Split an image string into host / repository / reference by
/// delegating to `oci_distribution::Reference`, the same validated
/// parser `platform-stack::oci` uses — so the OCI reference grammar
/// (lowercase repo names, `sha256:` digest format/length, `host:port`
/// vs tag disambiguation, the `library/` Docker-Hub default) is
/// enforced in exactly one place rather than re-derived here (and
/// rather than re-using `pull_secret::image_repo_path`, which is a
/// looser host/tag-split heuristic only meant to strip a tag).
///
/// `host` is `Reference::resolve_registry()`, which maps the
/// `docker.io` alias to the canonical `index.docker.io` manifest
/// endpoint the registry HEAD wants. The reference is a digest when
/// `digest()` is present, else the tag (the crate defaults a missing
/// tag to `latest`). A malformed input — e.g. `repo@sha256:abc` with a
/// too-short digest, or an uppercase repo name — is rejected here.
pub fn parse_image_ref(image: &str) -> Result<ImageRef, OciResolveError> {
    let image = image.trim();
    let reference: Reference = image
        .parse()
        .map_err(|_| OciResolveError::InvalidReference(image.to_string()))?;
    let (text, is_digest) = match (reference.digest(), reference.tag()) {
        (Some(digest), _) => (digest.to_string(), true),
        (None, Some(tag)) => (tag.to_string(), false),
        // `Reference` parsing guarantees one of tag/digest is set
        // (a bare name is defaulted to `:latest`), so this is
        // unreachable; fall back defensively rather than panic.
        (None, None) => ("latest".to_string(), false),
    };
    Ok(ImageRef {
        host: reference.resolve_registry().to_string(),
        repository: reference.repository().to_string(),
        reference: text,
        is_digest,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BearerChallenge {
    pub realm: String,
    pub service: Option<String>,
    pub scope: Option<String>,
}

impl BearerChallenge {
    /// `realm?service=<svc>&scope=<scope>` with the values percent-encoded.
    pub fn token_url(&self) -> String {
        let mut params: Vec<String> = Vec::new();
        if let Some(s) = &self.service {
            params.push(format!("service={}", urlencode(s)));
        }
        if let Some(s) = &self.scope {
            params.push(format!("scope={}", urlencode(s)));
        }
        if params.is_empty() {
            self.realm.clone()
        } else {
            format!("{}?{}", self.realm, params.join("&"))
        }
    }
}

/// Parse a `WWW-Authenticate: Bearer realm=...,service=...,scope=...`
/// header. Returns `None` for non-Bearer schemes (e.g. Basic).
pub fn parse_www_authenticate(header: &str) -> Option<BearerChallenge> {
    let rest = header.strip_prefix("Bearer ")?;
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for part in split_auth_params(rest) {
        let (k, v) = part.split_once('=')?;
        let v = v.trim().trim_matches('"').to_string();
        match k.trim() {
            "realm" => realm = Some(v),
            "service" => service = Some(v),
            "scope" => scope = Some(v),
            _ => {}
        }
    }
    Some(BearerChallenge {
        realm: realm?,
        service,
        scope,
    })
}

/// Split `k="v",k2="v2"` on commas that are NOT inside quotes.
fn split_auth_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_q = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                in_q = !in_q;
                buf.push(ch);
            }
            ',' if !in_q => {
                out.push(std::mem::take(&mut buf));
            }
            _ => buf.push(ch),
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

/// Minimal percent-encoding for the token query values (`:` `/` and
/// space are the only chars our scopes/services contain that need it).
fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

pub fn parse_token_response(body: &[u8]) -> Result<String, OciResolveError> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| OciResolveError::Auth(e.to_string()))?;
    v.get("token")
        .or_else(|| v.get("access_token"))
        .and_then(|t| t.as_str())
        .map(String::from)
        .ok_or_else(|| OciResolveError::Auth("token response had no token".into()))
}

/// Canonical Docker Hub manifest host that `parse_image_ref` (via
/// `Reference::resolve_registry()`) emits for the `docker.io` alias.
const DOCKER_HUB_HOST: &str = "index.docker.io";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryAuth {
    Anonymous,
    Basic(String, String),
}

/// Extract Basic auth for `host` from a kubelet `dockerconfigjson`
/// (`{"auths":{"<host>":{"auth":"<base64 user:pass>"}}}`). Falls back
/// to `Anonymous` when the host has no entry (public image with an
/// unrelated covering credential). `docker.io`/`index.docker.io`
/// aliases map to the canonical registry host.
pub fn auth_from_dockerconfigjson(dcj: &[u8], host: &str) -> Result<RegistryAuth, OciResolveError> {
    use base64::Engine;
    let v: serde_json::Value =
        serde_json::from_slice(dcj).map_err(|e| OciResolveError::Auth(e.to_string()))?;
    let auths = match v.get("auths").and_then(|a| a.as_object()) {
        Some(a) => a,
        None => return Ok(RegistryAuth::Anonymous),
    };
    let candidates = dockerhub_aliases(host);
    let entry = auths
        .iter()
        .find(|(k, _)| candidates.iter().any(|c| c == *k))
        .map(|(_, e)| e);
    let Some(entry) = entry else {
        return Ok(RegistryAuth::Anonymous);
    };
    let Some(b64) = entry.get("auth").and_then(|a| a.as_str()) else {
        return Ok(RegistryAuth::Anonymous);
    };
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| OciResolveError::Auth(e.to_string()))?;
    let decoded = String::from_utf8(decoded).map_err(|e| OciResolveError::Auth(e.to_string()))?;
    match decoded.split_once(':') {
        Some((u, p)) => Ok(RegistryAuth::Basic(u.to_string(), p.to_string())),
        None => Ok(RegistryAuth::Anonymous),
    }
}

fn dockerhub_aliases(host: &str) -> Vec<String> {
    if host == DOCKER_HUB_HOST || host == "docker.io" || host == "index.docker.io" {
        vec![
            DOCKER_HUB_HOST.to_string(),
            "docker.io".to_string(),
            "index.docker.io".to_string(),
            "https://index.docker.io/v1/".to_string(),
        ]
    } else {
        vec![host.to_string()]
    }
}

pub struct HttpReq<'a> {
    pub method: &'a str, // "HEAD" | "GET"
    pub url: String,
    pub accept: Option<&'a str>,
    pub bearer: Option<String>,
    pub basic: Option<(String, String)>,
}

pub struct HttpResp {
    pub status: u16,
    pub headers: HashMap<String, String>, // lowercased keys
    pub body: Vec<u8>,
}

/// Injected HTTP seam (mirrors grace.rs's injected clock): the flow is
/// unit-tested with a fake; production uses `ReqwestHttp`.
#[async_trait::async_trait]
pub trait RegistryHttp {
    async fn send(&self, req: HttpReq<'_>) -> Result<HttpResp, OciResolveError>;
}

const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
application/vnd.oci.image.manifest.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json, \
application/vnd.docker.distribution.manifest.v2+json";

/// Resolve `image` (a tag) to `repo@sha256:<digest>`. An already-digest
/// reference is returned verbatim (no I/O). On a 401 we run one bearer
/// token exchange and retry once.
pub async fn resolve_digest(
    http: &impl RegistryHttp,
    image: &str,
    auth: &RegistryAuth,
) -> Result<String, OciResolveError> {
    let r = parse_image_ref(image)?;
    if r.is_digest {
        return Ok(format!("{}/{}@{}", r.host, r.repository, r.reference));
    }
    let manifest_url = format!(
        "https://{}/v2/{}/manifests/{}",
        r.host, r.repository, r.reference
    );
    let basic = match auth {
        RegistryAuth::Basic(u, p) => Some((u.clone(), p.clone())),
        RegistryAuth::Anonymous => None,
    };

    let head = http
        .send(HttpReq {
            method: "HEAD",
            url: manifest_url.clone(),
            accept: Some(MANIFEST_ACCEPT),
            bearer: None,
            basic: basic.clone(),
        })
        .await?;

    let resp = match head.status {
        200 => head,
        401 => {
            let challenge = head
                .headers
                .get("www-authenticate")
                .and_then(|h| parse_www_authenticate(h))
                .ok_or_else(|| OciResolveError::Auth("401 without a Bearer challenge".into()))?;
            let token_resp = http
                .send(HttpReq {
                    method: "GET",
                    url: challenge.token_url(),
                    accept: None,
                    bearer: None,
                    basic: basic.clone(),
                })
                .await?;
            if token_resp.status != 200 {
                return Err(OciResolveError::Auth(format!(
                    "token endpoint returned {}",
                    token_resp.status
                )));
            }
            let token = parse_token_response(&token_resp.body)?;
            http.send(HttpReq {
                method: "HEAD",
                url: manifest_url.clone(),
                accept: Some(MANIFEST_ACCEPT),
                bearer: Some(token),
                basic: None,
            })
            .await?
        }
        s => {
            return Err(OciResolveError::Status {
                status: s,
                url: manifest_url,
            })
        }
    };

    if resp.status != 200 {
        return Err(OciResolveError::Status {
            status: resp.status,
            url: manifest_url,
        });
    }
    let digest = resp
        .headers
        .get("docker-content-digest")
        .ok_or(OciResolveError::NoDigestHeader)?;
    Ok(format!("{}/{}@{}", r.host, r.repository, digest))
}

pub struct ReqwestHttp {
    client: reqwest::Client,
}

impl ReqwestHttp {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
        }
    }
}

impl Default for ReqwestHttp {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl RegistryHttp for ReqwestHttp {
    async fn send(&self, req: HttpReq<'_>) -> Result<HttpResp, OciResolveError> {
        let method = match req.method {
            "HEAD" => reqwest::Method::HEAD,
            _ => reqwest::Method::GET,
        };
        let mut rb = self.client.request(method, &req.url);
        if let Some(a) = req.accept {
            rb = rb.header(reqwest::header::ACCEPT, a);
        }
        if let Some(t) = &req.bearer {
            rb = rb.bearer_auth(t);
        }
        if let Some((u, p)) = &req.basic {
            rb = rb.basic_auth(u, Some(p));
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| OciResolveError::Http(e.to_string()))?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        let body = resp
            .bytes()
            .await
            .map_err(|e| OciResolveError::Http(e.to_string()))?
            .to_vec();
        Ok(HttpResp {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(test)]
    fn base64_std(s: &str) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn auth_from_dockerconfigjson_matches_host() {
        // {"auths":{"ghcr.io":{"auth":"<base64 user:pass>"}}}
        let b64 = base64_std("alice:s3cret");
        let dcj = format!(r#"{{"auths":{{"ghcr.io":{{"auth":"{b64}"}}}}}}"#);
        let a = auth_from_dockerconfigjson(dcj.as_bytes(), "ghcr.io").unwrap();
        assert_eq!(a, RegistryAuth::Basic("alice".into(), "s3cret".into()));
    }

    #[test]
    fn auth_from_dockerconfigjson_no_match_is_anonymous() {
        let dcj = r#"{"auths":{"ghcr.io":{"auth":"eA=="}}}"#;
        assert_eq!(
            auth_from_dockerconfigjson(dcj.as_bytes(), "docker.io").unwrap(),
            RegistryAuth::Anonymous
        );
    }

    #[test]
    fn parse_image_ref_ghcr_with_tag() {
        let r = parse_image_ref("ghcr.io/acme/web:1.2.3").unwrap();
        assert_eq!(r.host, "ghcr.io");
        assert_eq!(r.repository, "acme/web");
        assert_eq!(r.reference, "1.2.3");
        assert!(!r.is_digest);
    }

    #[test]
    fn parse_image_ref_dockerhub_official_library_default_tag() {
        // No host => Docker Hub; single name => library/<name>; no tag => latest.
        // `resolve_registry()` maps the docker.io alias to its canonical
        // manifest endpoint.
        let r = parse_image_ref("nginx").unwrap();
        assert_eq!(r.host, "index.docker.io");
        assert_eq!(r.repository, "library/nginx");
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn parse_image_ref_dockerhub_namespaced() {
        let r = parse_image_ref("bitnami/redis:7").unwrap();
        assert_eq!(r.host, "index.docker.io");
        assert_eq!(r.repository, "bitnami/redis");
        assert_eq!(r.reference, "7");
    }

    #[test]
    fn parse_image_ref_already_digest_is_flagged() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let r = parse_image_ref(&format!("ghcr.io/acme/web@{digest}")).unwrap();
        assert!(r.is_digest);
        assert_eq!(r.reference, digest);
    }

    #[test]
    fn parse_image_ref_malformed_digest_is_rejected() {
        // The hand-rolled splitter silently accepted a 3-char digest;
        // delegating to oci_distribution validates the sha256 length.
        assert!(parse_image_ref("repo@sha256:abc").is_err());
    }

    #[test]
    fn parse_image_ref_uppercase_repo_is_rejected() {
        // OCI repository names must be lowercase — the validated parser
        // enforces this where the hand-rolled one did not.
        assert!(parse_image_ref("ghcr.io/Acme/Web:1.0").is_err());
    }

    #[test]
    fn parse_image_ref_localhost_port_is_a_registry_host() {
        let r = parse_image_ref("localhost:5000/team/app:dev").unwrap();
        assert_eq!(r.host, "localhost:5000");
        assert_eq!(r.repository, "team/app");
    }

    #[test]
    fn parse_image_ref_empty_is_error() {
        assert!(parse_image_ref("").is_err());
    }

    #[test]
    fn parse_www_authenticate_bearer_challenge() {
        let h = r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:acme/web:pull""#;
        let c = parse_www_authenticate(h).unwrap();
        assert_eq!(c.realm, "https://ghcr.io/token");
        assert_eq!(c.service.as_deref(), Some("ghcr.io"));
        assert_eq!(c.scope.as_deref(), Some("repository:acme/web:pull"));
    }

    #[test]
    fn parse_www_authenticate_non_bearer_is_none() {
        assert!(parse_www_authenticate("Basic realm=\"x\"").is_none());
    }

    #[test]
    fn token_url_builds_query_from_challenge() {
        let c = BearerChallenge {
            realm: "https://ghcr.io/token".into(),
            service: Some("ghcr.io".into()),
            scope: Some("repository:acme/web:pull".into()),
        };
        let url = c.token_url();
        assert!(url.starts_with("https://ghcr.io/token?"));
        assert!(url.contains("service=ghcr.io"));
        assert!(url.contains("scope=repository%3Aacme%2Fweb%3Apull"));
    }

    #[test]
    fn parse_token_response_accepts_token_and_access_token() {
        assert_eq!(parse_token_response(br#"{"token":"abc"}"#).unwrap(), "abc");
        assert_eq!(
            parse_token_response(br#"{"access_token":"xyz"}"#).unwrap(),
            "xyz"
        );
        assert!(parse_token_response(br#"{}"#).is_err());
    }

    // (scripted-key, status, headers, body) — aliased to keep
    // clippy::type_complexity quiet on the `FakeHttp.responses` field.
    type ScriptedResponse = (String, u16, Vec<(String, String)>, Vec<u8>);

    #[derive(Default)]
    struct FakeHttp {
        // url -> (status, headers, body), scripted per call sequence is not
        // needed: keyed by (method, url, has_bearer).
        responses: Vec<ScriptedResponse>,
    }
    #[async_trait::async_trait]
    impl RegistryHttp for FakeHttp {
        async fn send(&self, req: HttpReq<'_>) -> Result<HttpResp, OciResolveError> {
            let key = format!("{} {} bearer={}", req.method, req.url, req.bearer.is_some());
            for (k, status, headers, body) in &self.responses {
                if *k == key {
                    return Ok(HttpResp {
                        status: *status,
                        headers: headers.iter().cloned().collect(),
                        body: body.clone(),
                    });
                }
            }
            Err(OciResolveError::Http(format!(
                "no scripted response for {key}"
            )))
        }
    }

    #[tokio::test]
    async fn resolve_digest_public_200_returns_digest() {
        let url = "https://ghcr.io/v2/acme/web/manifests/1.0";
        let http = FakeHttp {
            responses: vec![(
                format!("HEAD {url} bearer=false"),
                200,
                vec![("docker-content-digest".into(), "sha256:deadbeef".into())],
                vec![],
            )],
        };
        let got = resolve_digest(&http, "ghcr.io/acme/web:1.0", &RegistryAuth::Anonymous)
            .await
            .unwrap();
        assert_eq!(got, "ghcr.io/acme/web@sha256:deadbeef");
    }

    #[tokio::test]
    async fn resolve_digest_401_then_token_then_200() {
        let m = "https://ghcr.io/v2/acme/web/manifests/1.0";
        let tok = "https://ghcr.io/token?service=ghcr.io&scope=repository%3Aacme%2Fweb%3Apull";
        let http = FakeHttp {
            responses: vec![
                (
                    format!("HEAD {m} bearer=false"),
                    401,
                    vec![(
                        "www-authenticate".into(),
                        r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:acme/web:pull""#.into(),
                    )],
                    vec![],
                ),
                (format!("GET {tok} bearer=false"), 200, vec![], br#"{"token":"T"}"#.to_vec()),
                (
                    format!("HEAD {m} bearer=true"),
                    200,
                    vec![("docker-content-digest".into(), "sha256:cafe".into())],
                    vec![],
                ),
            ],
        };
        let got = resolve_digest(&http, "ghcr.io/acme/web:1.0", &RegistryAuth::Anonymous)
            .await
            .unwrap();
        assert_eq!(got, "ghcr.io/acme/web@sha256:cafe");
    }

    #[tokio::test]
    async fn resolve_digest_passthrough_when_already_digest() {
        // An already-digest reference needs no lookup. The digest must be a
        // valid 64-hex sha256 (oci_distribution::Reference enforces this);
        // resolve_digest parses the ref first, so a malformed digest would
        // fail resolution rather than pass through.
        let d = format!("sha256:{}", "a".repeat(64));
        let image = format!("ghcr.io/acme/web@{d}");
        let http = FakeHttp::default();
        let got = resolve_digest(&http, &image, &RegistryAuth::Anonymous)
            .await
            .unwrap();
        assert_eq!(got, format!("ghcr.io/acme/web@{d}"));
    }

    #[tokio::test]
    async fn resolve_digest_404_is_error() {
        let url = "https://ghcr.io/v2/acme/web/manifests/nope";
        let http = FakeHttp {
            responses: vec![(format!("HEAD {url} bearer=false"), 404, vec![], vec![])],
        };
        let e = resolve_digest(&http, "ghcr.io/acme/web:nope", &RegistryAuth::Anonymous)
            .await
            .unwrap_err();
        assert!(matches!(e, OciResolveError::Status { status: 404, .. }));
    }
}
