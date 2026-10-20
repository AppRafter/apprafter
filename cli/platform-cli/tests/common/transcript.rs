// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A conversation at a pseudo-terminal as a golden (D.4b-e1a, G-LOCKON-D4A): its exit, what
//! the terminal showed and what reached stdout, normalised so two runs — a plain build's and
//! a test build's included — compare byte for byte. The body only, no command line, so one
//! golden can pin two invocations that must say the same (`lock on` and `lock on
//! --lock-password` before the default moved). Normalised: carriage returns (the terminal's
//! `\r\n`), a test build's one line (`vault_access::insecure_kdf_note`), the recovery key's
//! groups and its slot's id (random per run), and the sandbox root. Nothing else: every other
//! byte is the CLI's.
//! Recorded and checked as every golden is (`common::golden::check`; record with
//! `APPRAFTER_GOLDEN_UPDATE=1`). Unix, as the pseudo-terminal is.

use super::golden::{check, section};
use super::spawn::Spawn;
use super::terminal::Ended;

/// The line a test build prints first; a plain build does not.
pub const TEST_BUILD_LINE: &str =
    "INSECURE-FAST-KDF: this test build derives keys with weak parameters\n";

/// The line a recovery key is shown under (`commands::lock::recovery_text`).
const KEY_PREFIX: &str = "APPRAFTER-RECOVERY-KEY-1";

/// `text` with what varies per run masked (module docs).
pub fn normalize(sp: &Spawn, text: &str) -> String {
    let text = text.replace('\r', "").replace(TEST_BUILD_LINE, "");
    let mut groups = Vec::new();
    let mut lines = text.lines().map(str::trim);
    while let Some(line) = lines.next() {
        if line == KEY_PREFIX {
            if let Some(next) = lines.find(|l| !l.is_empty()) {
                groups.push(next.to_string());
            }
        }
    }
    let mut out = text.clone();
    for g in groups {
        out = out.replace(&g, "<RECOVERY-KEY>");
    }
    sp.mask_root(&mask_slot_ids(&out))
}

/// "slot " and eight lowercase hex characters — a recovery key's slot id (D.4b B80), random
/// per run — as "slot <SLOT>"; a longer run of hex is left as it is.
fn mask_slot_ids(text: &str) -> String {
    const AT: &str = "slot ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(AT) {
        out.push_str(&rest[..at + AT.len()]);
        let after = &rest[at + AT.len()..];
        let bytes = after.as_bytes();
        let id = bytes.len() >= 8
            && bytes[..8]
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            && !bytes.get(8).is_some_and(u8::is_ascii_hexdigit);
        if id {
            out.push_str("<SLOT>");
            rest = &after[8..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// The slot id a `lock on` showed with its recovery key (D.4b B80).
pub fn shown_slot_id(terminal: &str) -> String {
    const AT: &str = "· slot ";
    let at = terminal.find(AT).expect("a slot id was shown") + AT.len();
    terminal[at..at + 8].to_string()
}

/// The golden body of a conversation (module docs).
pub fn transcript(sp: &Spawn, end: &Ended) -> String {
    let mut doc = match end.status.code() {
        Some(code) => format!("[exit {code}]\n"),
        None => "[exit signal]\n".to_string(),
    };
    section(&mut doc, "terminal", &normalize(sp, &end.terminal));
    section(&mut doc, "stdout", &normalize(sp, &end.stdout));
    doc
}

/// `end`'s transcript against `tests/golden/<case>.golden`; the sandbox root must not survive.
pub fn check_transcript(case: &str, sp: &Spawn, end: &Ended) {
    let doc = transcript(sp, end);
    let name = sp.root().file_name().expect("the root's name");
    let name = name.to_string_lossy();
    assert!(
        !doc.contains(name.as_ref()),
        "golden `{case}`: the sandbox root survived normalisation:\n{doc}"
    );
    check(case, &doc);
}
