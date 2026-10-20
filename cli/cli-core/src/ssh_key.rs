// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What a target's SSH key must be before its path is saved or its body is sent to the provider
//! (GOTCHA-149): one OpenSSH public key line, `<type> <base64> [comment]`. The private key sits
//! next to the `.pub`, one dropped suffix away, and a provider is sent whatever the key file
//! holds — so anything else is refused, and a private key by name.

use std::path::Path;

use crate::error::CliError;

/// How a refusal names a key file: `` SSH key `<path>` ``.
pub fn file_origin(path: &Path) -> String {
    format!("SSH key `{}`", path.display())
}

/// Why a body is not an OpenSSH public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotAPublicKey {
    /// A private key: a PEM block `-----BEGIN … PRIVATE KEY-----` (OpenSSH's own format, RSA,
    /// EC, PKCS#8, encrypted or not) or a PuTTY `.ppk`.
    PrivateKey,
    /// Anything else: not exactly one `<type> <base64> [comment]` line of a known type.
    Other,
}

/// Where a refused SSH key came from. Its refusal names it by `origin`; what fixes it depends on
/// this (D.3d verification): `apply` takes the manifest's `sshKeys` first, then
/// `APPRAFTER_SSH_PUBLIC_KEY`, then the target's stored path, so pointing the target at another
/// key fixes neither of the first two, and a first `target add` has no target to `--renew`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// A key file given to `target add` for a target that does not exist yet.
    NewTargetFile,
    /// A key file given for an existing target (`--renew`, `--force`), or the one it stores.
    TargetFile,
    /// The key text in `APPRAFTER_SSH_PUBLIC_KEY`.
    Env,
    /// The Infrastructure manifest's `sshKeys[index]`.
    Manifest { index: usize },
}

impl NotAPublicKey {
    /// The refusal of the key `origin` names ([`file_origin`] for a file), from `source`.
    pub fn refusal(self, origin: impl Into<String>, source: KeySource) -> CliError {
        CliError::SshKeyNotPublic {
            origin: origin.into(),
            private_key: self == Self::PrivateKey,
            from: source,
        }
    }
}

/// An OpenSSH public key line's type and comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKeyLine {
    pub algo: String,
    pub comment: Option<String>,
}

/// `body` as an OpenSSH public key: one line (surrounding whitespace and a trailing newline
/// aside) of a known type — ssh-ed25519, ssh-rsa, `ecdsa-sha2-<curve>`,
/// sk-ssh-ed25519@openssh.com or `sk-ecdsa-sha2-<curve>@openssh.com` — and a base64 key, then an
/// optional comment.
pub fn parse_public_key(body: &str) -> Result<PublicKeyLine, NotAPublicKey> {
    let text = body.trim();
    if is_private_key(text) {
        return Err(NotAPublicKey::PrivateKey);
    }
    if text.lines().count() != 1 {
        return Err(NotAPublicKey::Other);
    }
    let mut fields = text.split_whitespace();
    let (Some(algo), Some(key)) = (fields.next(), fields.next()) else {
        return Err(NotAPublicKey::Other);
    };
    if !is_public_key_type(algo) || !is_base64(key) {
        return Err(NotAPublicKey::Other);
    }
    let comment = fields.collect::<Vec<_>>().join(" ");
    Ok(PublicKeyLine {
        algo: algo.to_string(),
        comment: (!comment.is_empty()).then_some(comment),
    })
}

/// A PEM private key block anywhere in the text, or a PuTTY private key file.
fn is_private_key(text: &str) -> bool {
    text.starts_with("PuTTY-User-Key-File-")
        || text.lines().any(|line| {
            let line = line.trim();
            line.starts_with("-----BEGIN") && line.contains("PRIVATE KEY")
        })
}

fn is_public_key_type(algo: &str) -> bool {
    let curve = |name: &str| !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric());
    matches!(
        algo,
        "ssh-ed25519" | "ssh-rsa" | "sk-ssh-ed25519@openssh.com"
    ) || algo.strip_prefix("ecdsa-sha2-").is_some_and(curve)
        || algo
            .strip_prefix("sk-ecdsa-sha2-")
            .and_then(|rest| rest.strip_suffix("@openssh.com"))
            .is_some_and(curve)
}

/// Standard base64 with its padding: a whole number of 4-character groups, at most two `=`,
/// only at the end.
fn is_base64(text: &str) -> bool {
    let data = text.trim_end_matches('=');
    !data.is_empty()
        && text.len().is_multiple_of(4)
        && text.len() - data.len() <= 2
        && data
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One real public key of each family (`ssh-keygen -t …`, comment changed), the shape each
    /// type's `.pub` has.
    const PUBLIC_KEYS: [(&str, &str); 5] = [
        (
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBkpKvtzRjM2Y0ZULrPOF7N0ZAAXjBXw0n4kz2ZP1t3T alex@work\n",
            "ssh-ed25519",
        ),
        (
            "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAAgQC3m3rS0s7QWlR0dxBHOWdVmMH+g5j0/o== alex@work",
            "ssh-rsa",
        ),
        (
            "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEh0 alex@work",
            "ecdsa-sha2-nistp256",
        ),
        (
            "sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIA== alex@yubikey",
            "sk-ssh-ed25519@openssh.com",
        ),
        (
            "sk-ecdsa-sha2-nistp256@openssh.com AAAAInNrLWVjZHNhLXNoYTItbmlzdHAyNTZAb3BlbnNzaC5jb20= alex@yubikey",
            "sk-ecdsa-sha2-nistp256@openssh.com",
        ),
    ];

    #[test]
    fn a_public_key_of_each_family_reads_with_its_type_and_comment() {
        for (body, algo) in PUBLIC_KEYS {
            let line = parse_public_key(body).unwrap_or_else(|e| panic!("{body}: {e:?}"));
            assert_eq!(line.algo, algo);
            assert!(line
                .comment
                .as_deref()
                .is_some_and(|c| c.starts_with("alex@")));
        }
        assert_eq!(
            parse_public_key("  ssh-ed25519 AAAA\r\n").unwrap(),
            PublicKeyLine {
                algo: "ssh-ed25519".into(),
                comment: None
            },
            "no comment, CRLF and surrounding blanks"
        );
        assert_eq!(
            parse_public_key("ssh-ed25519 AAAA two words")
                .unwrap()
                .comment,
            Some("two words".into())
        );
    }

    #[test]
    fn a_private_key_is_refused_by_name_whatever_its_format() {
        for body in [
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQ==\n-----END OPENSSH PRIVATE KEY-----\n",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\n-----END RSA PRIVATE KEY-----\n",
            "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEI\n-----END EC PRIVATE KEY-----",
            "-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMG\n-----END PRIVATE KEY-----",
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIFHDBOBgkq\n-----END ENCRYPTED PRIVATE KEY-----",
            "PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: none\nComment: alex\n",
            // a stray line in front of the block does not hide it
            "# my key\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----",
        ] {
            assert_eq!(
                parse_public_key(body),
                Err(NotAPublicKey::PrivateKey),
                "{body}"
            );
        }
    }

    #[test]
    fn anything_else_is_not_a_public_key() {
        for body in [
            "",
            "   \n",
            "not-a-key",
            "ssh-ed25519",
            // a type outside the list (DSA is retired; a certificate is not a key)
            "ssh-dss AAAA alex@old",
            "ssh-ed25519-cert-v01@openssh.com AAAA alex",
            "ecdsa-sha2- AAAA",
            "sk-ecdsa-sha2-nistp256 AAAA",
            // the key is not base64
            "ssh-ed25519 AAAA-from-file me@file",
            "ssh-ed25519 AAA",
            "ssh-ed25519 AAAAA===",
            "ssh-ed25519 ====",
            "ssh-ed25519 AA=A",
            // a public key in RFC 4716's own format, not OpenSSH's line
            "---- BEGIN SSH2 PUBLIC KEY ----\nAAAAC3NzaC1lZDI1NTE5\n---- END SSH2 PUBLIC KEY ----",
            // two keys: a provider takes one
            "ssh-ed25519 AAAA one\nssh-ed25519 BBBB two\n",
            // an authorized_keys line with options
            "command=\"x\" ssh-ed25519 AAAA alex",
        ] {
            assert_eq!(
                parse_public_key(body),
                Err(NotAPublicKey::Other),
                "{body:?}"
            );
        }
    }
}
