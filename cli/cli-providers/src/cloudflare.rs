// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Fetch Cloudflare's published IPv4 + IPv6 CIDR ranges (the origin-firewall
//! allowlist, 1.83d). Provider-agnostic; the Hetzner firewall builder consumes
//! the union. FAIL-FAST: a stale allowlist is worse than a failed apply, so
//! there are NO hardcoded fallback CIDRs — any fetch problem is an error.

use cli_core::{CliError, Result};

/// CF publishes its ranges as plain text, one CIDR per line.
const CF_IPS_V4_URL: &str = "https://www.cloudflare.com/ips-v4";
const CF_IPS_V6_URL: &str = "https://www.cloudflare.com/ips-v6";

/// Seam so tests inject fixed bodies instead of hitting the network.
pub trait CloudflareIpSource {
    /// Return the body of a Cloudflare IP-ranges endpoint (`url`).
    fn get(&self, url: &str) -> Result<String>;
}

/// The real source — one `ureq` GET per URL. Non-200 / transport error map to
/// `CliError`, so `fetch_cloudflare_ips` can fail-fast on either.
pub struct UreqCloudflareIpSource;

impl CloudflareIpSource for UreqCloudflareIpSource {
    fn get(&self, url: &str) -> Result<String> {
        match ureq::get(url).set("Accept", "text/plain").call() {
            Ok(r) => r
                .into_string()
                .map_err(|e| CliError::Other(format!("read Cloudflare body from {url}: {e}"))),
            Err(ureq::Error::Status(status, _)) => Err(CliError::Other(format!(
                "Cloudflare {url} returned HTTP {status}"
            ))),
            Err(ureq::Error::Transport(t)) => Err(CliError::Other(format!(
                "transport error fetching {url}: {t}"
            ))),
        }
    }
}

/// Parse one CF endpoint body into trimmed, non-empty CIDR lines.
fn parse_cidrs(body: &str) -> Vec<String> {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Fetch + union Cloudflare's IPv4 then IPv6 CIDR ranges. FAIL-FAST: any
/// fetch error OR an empty (all-blank) endpoint aborts with the canonical
/// message — NO hardcoded fallback. Each family must contribute at least one
/// CIDR; an empty v4 (or v6) list would silently drop that family's allowlist.
pub fn fetch_cloudflare_ips(src: &impl CloudflareIpSource) -> Result<Vec<String>> {
    let fail = |detail: String| {
        CliError::Other(format!(
            "cannot fetch Cloudflare IP ranges; aborting to avoid stale firewall ({detail})"
        ))
    };
    let v4 = src
        .get(CF_IPS_V4_URL)
        .map_err(|e| fail(format!("ips-v4: {e}")))?;
    let v6 = src
        .get(CF_IPS_V6_URL)
        .map_err(|e| fail(format!("ips-v6: {e}")))?;
    let v4_cidrs = parse_cidrs(&v4);
    let v6_cidrs = parse_cidrs(&v6);
    if v4_cidrs.is_empty() {
        return Err(fail("ips-v4 returned no CIDRs".to_string()));
    }
    if v6_cidrs.is_empty() {
        return Err(fail("ips-v6 returned no CIDRs".to_string()));
    }
    let mut out = v4_cidrs;
    out.extend(v6_cidrs);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Test source returning canned bodies (or an error) per URL.
    struct MockSource {
        bodies: HashMap<String, std::result::Result<String, String>>,
        calls: RefCell<Vec<String>>,
    }
    impl MockSource {
        fn new() -> Self {
            Self {
                bodies: HashMap::new(),
                calls: RefCell::new(Vec::new()),
            }
        }
        fn ok(mut self, url: &str, body: &str) -> Self {
            self.bodies.insert(url.into(), Ok(body.into()));
            self
        }
        fn err(mut self, url: &str, msg: &str) -> Self {
            self.bodies.insert(url.into(), Err(msg.into()));
            self
        }
    }
    impl CloudflareIpSource for MockSource {
        fn get(&self, url: &str) -> Result<String> {
            self.calls.borrow_mut().push(url.to_string());
            match self.bodies.get(url) {
                Some(Ok(b)) => Ok(b.clone()),
                Some(Err(m)) => Err(CliError::Other(m.clone())),
                None => Err(CliError::Other(format!("no canned body for {url}"))),
            }
        }
    }

    #[test]
    fn fetches_and_unions_v4_then_v6() {
        let src = MockSource::new()
            .ok(CF_IPS_V4_URL, "173.245.48.0/20\n103.21.244.0/22\n")
            .ok(CF_IPS_V6_URL, "2400:cb00::/32\n2606:4700::/32\n");
        let ips = fetch_cloudflare_ips(&src).unwrap();
        assert_eq!(
            ips,
            vec![
                "173.245.48.0/20".to_string(),
                "103.21.244.0/22".to_string(),
                "2400:cb00::/32".to_string(),
                "2606:4700::/32".to_string(),
            ]
        );
    }

    #[test]
    fn ignores_blank_lines_and_trims() {
        let src = MockSource::new()
            .ok(CF_IPS_V4_URL, "  173.245.48.0/20  \n\n\n")
            .ok(CF_IPS_V6_URL, "2400:cb00::/32\n");
        let ips = fetch_cloudflare_ips(&src).unwrap();
        assert_eq!(
            ips,
            vec!["173.245.48.0/20".to_string(), "2400:cb00::/32".to_string()]
        );
    }

    #[test]
    fn fail_fast_on_v4_error() {
        let src = MockSource::new()
            .err(CF_IPS_V4_URL, "boom")
            .ok(CF_IPS_V6_URL, "2400:cb00::/32\n");
        let err = fetch_cloudflare_ips(&src).unwrap_err();
        assert!(format!("{err}").contains("cannot fetch Cloudflare IP ranges"));
    }

    #[test]
    fn fail_fast_on_empty_body() {
        let src = MockSource::new()
            .ok(CF_IPS_V4_URL, "\n  \n")
            .ok(CF_IPS_V6_URL, "2400:cb00::/32\n");
        let err = fetch_cloudflare_ips(&src).unwrap_err();
        assert!(format!("{err}").contains("cannot fetch Cloudflare IP ranges"));
    }
}
