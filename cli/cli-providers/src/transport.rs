// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `ureq` transport failures, told once for every HTTP client here: what the failure says, and
//! whether any answer came back.

use ureq::ErrorKind;

/// ureq's own account of `t` (its `Display`) without the URL it starts with: every message
/// built from it names the URL it asked for already (WI-453: a doctor row read "talking to X:
/// X: Connection Failed"). ureq reports the URL asked for even when a redirect failed
/// elsewhere, so nothing is lost.
pub(crate) fn detail(t: &ureq::Transport) -> String {
    let mut out = t.kind().to_string();
    if let Some(message) = t.message() {
        out.push_str(": ");
        out.push_str(message);
    }
    if let Some(source) = std::error::Error::source(t) {
        out.push_str(": ");
        out.push_str(&source.to_string());
    }
    out
}

/// Whether the request got no answer: the name did not resolve, the connection (or a proxy's)
/// failed or dropped, or it timed out — ureq reports a timeout as `Io`. Not what ureq refuses
/// before it sends (a malformed URL, an unknown scheme, a bad proxy setting) nor an answer it
/// cannot read (a bad status line or header, too many redirects).
pub(crate) fn no_answer(kind: ErrorKind) -> bool {
    match kind {
        ErrorKind::Dns | ErrorKind::ConnectionFailed | ErrorKind::Io | ErrorKind::ProxyConnect => {
            true
        }
        ErrorKind::InvalidUrl
        | ErrorKind::UnknownScheme
        | ErrorKind::InsecureRequestHttpsOnly
        | ErrorKind::TooManyRedirects
        | ErrorKind::BadStatus
        | ErrorKind::BadHeader
        | ErrorKind::InvalidProxyUrl
        | ErrorKind::ProxyUnauthorized
        | ErrorKind::HTTP => false,
    }
}
