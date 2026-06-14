// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Parse + validate imported TLS certificates for `apprafter target cert
//! import` (1.83e). Pure — no I/O. RSA-only (Cloudflare Origin CA certs are
//! RSA 2048); ECDSA/EdDSA deferred to full 4.1b.

use chrono::{DateTime, Utc};
use cli_core::{CliError, Result};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::{RsaPrivateKey, RsaPublicKey};
use x509_cert::der::{DecodePem, Encode};
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::Certificate;

/// A validated imported certificate — the parse output the command consumes.
pub struct ImportedCert {
    pub sans: Vec<String>,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    pub cert_pem: String,
    pub key_pem: String,
}

// Manual `Debug` that NEVER prints `key_pem` (the private key) — so an
// accidental `{:?}`/`debug!` of an `ImportedCert` can't leak it. `cert_pem`
// is public material but elided too (it's bulky); the SANs + validity carry
// the diagnostic value. `Result::unwrap_err()` in tests needs this impl.
impl std::fmt::Debug for ImportedCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedCert")
            .field("sans", &self.sans)
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .field("cert_pem", &"<elided>")
            .field("key_pem", &"<redacted>")
            .finish()
    }
}

/// Parse + validate a PEM cert against its PEM key. Errors BEFORE any cluster
/// write: malformed PEM, non-RSA key, cert↔key mismatch, no DNS SAN. Does NOT
/// check expiry — that's `expiry_status` (caller injects `now`).
pub fn parse_and_validate(cert_pem: &str, key_pem: &str) -> Result<ImportedCert> {
    let cert = Certificate::from_pem(cert_pem.as_bytes())
        .map_err(|e| CliError::Other(format!("parse certificate PEM: {e}")))?;

    let sans = dns_sans(&cert)?;
    if sans.is_empty() {
        return Err(CliError::Other(
            "certificate has no DNS SANs — a Gateway listener can't match it".to_string(),
        ));
    }

    let spki_der = cert
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|e| CliError::Other(format!("encode certificate SPKI: {e}")))?;
    let cert_pub = RsaPublicKey::from_public_key_der(&spki_der).map_err(|_| {
        CliError::Other(
            "unsupported certificate key type (RSA only in this slice); \
             ECDSA/EdDSA deferred to 4.1b"
                .to_string(),
        )
    })?;
    let key = parse_rsa_private_key(key_pem)?;
    if key.to_public_key() != cert_pub {
        return Err(CliError::Other(
            "certificate and private key do not match".to_string(),
        ));
    }

    Ok(ImportedCert {
        sans,
        not_before: time_to_utc(&cert.tbs_certificate.validity.not_before),
        not_after: time_to_utc(&cert.tbs_certificate.validity.not_after),
        cert_pem: cert_pem.to_string(),
        key_pem: key_pem.to_string(),
    })
}

/// Parse an RSA private key from PKCS#8 ("PRIVATE KEY") or PKCS#1 ("RSA PRIVATE
/// KEY") PEM. A non-RSA key fails both -> the "unsupported" error.
fn parse_rsa_private_key(key_pem: &str) -> Result<RsaPrivateKey> {
    if let Ok(k) = RsaPrivateKey::from_pkcs8_pem(key_pem) {
        return Ok(k);
    }
    RsaPrivateKey::from_pkcs1_pem(key_pem).map_err(|_| {
        CliError::Other(
            "unsupported private key type (RSA only in this slice); \
             ECDSA/EdDSA deferred to 4.1b"
                .to_string(),
        )
    })
}

fn dns_sans(cert: &Certificate) -> Result<Vec<String>> {
    let Some((_critical, san)) = cert
        .tbs_certificate
        .get::<SubjectAltName>()
        .map_err(|e| CliError::Other(format!("decode SubjectAltName: {e}")))?
    else {
        return Ok(Vec::new());
    };
    Ok(san
        .0
        .iter()
        .filter_map(|gn| match gn {
            GeneralName::DnsName(dns) => Some(dns.to_string()),
            _ => None,
        })
        .collect())
}

fn time_to_utc(t: &x509_cert::time::Time) -> DateTime<Utc> {
    DateTime::<Utc>::from(t.to_system_time())
}

/// Validity classification of a cert at instant `now` (injected for tests).
pub enum ExpiryStatus {
    Expired,
    NotYetValid,
    NearExpiry { days: i64 },
    Ok,
}

/// Pure expiry classification. `< 30` days remaining warns (not rejects);
/// already-expired / not-yet-valid is a hard error at the call site.
pub fn expiry_status(
    not_before: DateTime<Utc>,
    not_after: DateTime<Utc>,
    now: DateTime<Utc>,
) -> ExpiryStatus {
    if now < not_before {
        return ExpiryStatus::NotYetValid;
    }
    if now >= not_after {
        return ExpiryStatus::Expired;
    }
    let days = (not_after - now).num_days();
    if days < 30 {
        ExpiryStatus::NearExpiry { days }
    } else {
        ExpiryStatus::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_CRT: &str = include_str!("../tests/fixtures/valid.crt");
    const VALID_KEY: &str = include_str!("../tests/fixtures/valid.key");
    const OTHER_KEY: &str = include_str!("../tests/fixtures/other.key");
    const EC_KEY: &str = include_str!("../tests/fixtures/ec.key");
    const NOSAN_CRT: &str = include_str!("../tests/fixtures/nosan.crt");

    #[test]
    fn parses_valid_cert_extracts_sans_and_validity() {
        let c = parse_and_validate(VALID_CRT, VALID_KEY).unwrap();
        assert!(c.sans.contains(&"apprafter.dev".to_string()));
        assert!(c.sans.contains(&"*.apprafter.dev".to_string()));
        assert!(c.not_after > c.not_before);
        assert_eq!(c.cert_pem, VALID_CRT);
        assert_eq!(c.key_pem, VALID_KEY);
    }

    #[test]
    fn rejects_cert_key_mismatch() {
        let err = parse_and_validate(VALID_CRT, OTHER_KEY).unwrap_err();
        assert!(format!("{err}").contains("do not match"));
    }

    #[test]
    fn rejects_non_rsa_key() {
        let err = parse_and_validate(VALID_CRT, EC_KEY).unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("unsupported"));
    }

    #[test]
    fn rejects_cert_without_dns_sans() {
        let err = parse_and_validate(NOSAN_CRT, VALID_KEY).unwrap_err();
        assert!(format!("{err}").contains("no DNS SANs"));
    }

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn expiry_status_classifies_window() {
        let nb = dt("2026-01-01T00:00:00Z");
        let na = dt("2027-01-01T00:00:00Z");
        // before notBefore
        assert!(matches!(
            expiry_status(nb, na, dt("2025-12-01T00:00:00Z")),
            ExpiryStatus::NotYetValid
        ));
        // after notAfter
        assert!(matches!(
            expiry_status(nb, na, dt("2027-02-01T00:00:00Z")),
            ExpiryStatus::Expired
        ));
        // <30 days left
        assert!(matches!(
            expiry_status(nb, na, dt("2026-12-20T00:00:00Z")),
            ExpiryStatus::NearExpiry { days } if (0..30).contains(&days)
        ));
        // comfortably valid
        assert!(matches!(
            expiry_status(nb, na, dt("2026-06-01T00:00:00Z")),
            ExpiryStatus::Ok
        ));
    }
}
