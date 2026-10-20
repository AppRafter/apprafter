// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Normalisers for a CLI run's output, on every OS: the golden harness (`golden.rs`, Unix) and
//! T-lazy-1 (`laziness_test.rs`, every OS) mask what varies per run with them.

/// Replace every RFC 3339 UTC timestamp, `YYYY-MM-DDTHH:MM:SS[.fraction]Z`
/// (the shape tracing's default timer prints), with `<TS>`.
pub fn mask_timestamps(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    // Bytes of `s` already copied (or masked) into `out`.
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        // A timestamp never continues a longer digit run.
        let starts_token = at == 0 || !bytes[at - 1].is_ascii_digit();
        match timestamp_len(&bytes[at..]) {
            Some(len) if starts_token => {
                out.push_str(&s[copied..at]);
                out.push_str("<TS>");
                at += len;
                copied = at;
            }
            _ => at += 1,
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// The length of the `YYYY-MM-DDTHH:MM:SS[.fraction]Z` timestamp that
/// `bytes` starts with, if it starts with one.
pub fn timestamp_len(bytes: &[u8]) -> Option<usize> {
    // `9` stands for any ASCII digit; everything else is literal.
    const SHAPE: &[u8] = b"9999-99-99T99:99:99";
    let head = bytes.get(..SHAPE.len())?;
    let fits = head.iter().zip(SHAPE).all(|(b, want)| match want {
        b'9' => b.is_ascii_digit(),
        literal => b == literal,
    });
    if !fits {
        return None;
    }
    let mut len = SHAPE.len();
    if bytes.get(len) == Some(&b'.') {
        let digits = bytes[len + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits == 0 {
            return None;
        }
        len += 1 + digits;
    }
    (bytes.get(len) == Some(&b'Z')).then_some(len + 1)
}

/// `<digits> ms` → `<MS> ms` where the digits start a token (start of text, or after a byte
/// that is neither alphanumeric nor `.`) and `ms` ends one: timings (`182 ms`) vary per run.
pub fn mask_millis(s: &str) -> String {
    let bytes = s.as_bytes();
    let (mut out, mut copied, mut at) = (String::with_capacity(s.len()), 0, 0);
    while at < bytes.len() {
        let starts = bytes[at].is_ascii_digit()
            && (at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'.'));
        if !starts {
            at += 1;
            continue;
        }
        let digits = bytes[at..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        let rest = &bytes[at + digits..];
        if rest.starts_with(b" ms") && rest.get(3).is_none_or(|b| !b.is_ascii_alphanumeric()) {
            out.push_str(&s[copied..at]);
            out.push_str("<MS> ms");
            at += digits + 3;
            copied = at;
        } else {
            at += digits;
        }
    }
    out.push_str(&s[copied..]);
    out
}
