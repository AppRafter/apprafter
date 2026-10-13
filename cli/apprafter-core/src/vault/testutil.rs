// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Helpers of the vault's unit tests: hex in and out, the vectors' fixed "random" bytes, and
//! the v1 vector directory.

use std::path::PathBuf;

/// `text` as bytes; whitespace is ignored, so a long value can be laid out in lines.
pub(crate) fn unhex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    assert!(
        digits.len().is_multiple_of(2),
        "odd hex length {}",
        digits.len()
    );
    digits
        .chunks(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("hex is ASCII");
            u8::from_str_radix(pair, 16).unwrap_or_else(|e| panic!("bad hex {pair:?}: {e}"))
        })
        .collect()
}

/// `bytes` as lower-case hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `n` bytes counting up from `start`, wrapping: the vectors' fixed "random" inputs.
pub(crate) fn seq(start: u8, n: usize) -> Vec<u8> {
    (0..n).map(|i| start.wrapping_add(i as u8)).collect()
}

/// [`seq`] as an array.
pub(crate) fn seq_array<const N: usize>(start: u8) -> [u8; N] {
    seq(start, N).try_into().expect("N bytes")
}

/// `cli/apprafter-core/tests/vault_vectors/v1/<rel>`.
pub(crate) fn vector_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("vault_vectors")
        .join("v1")
        .join(rel)
}

/// A payload plaintext built by hand, entries in the order given.
pub(crate) fn tlv(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = (entries.len() as u16).to_le_bytes().to_vec();
    for (key, value) in entries {
        out.push(key.len() as u8);
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(&(value.len() as u16).to_le_bytes());
        out.extend_from_slice(value);
    }
    out
}
