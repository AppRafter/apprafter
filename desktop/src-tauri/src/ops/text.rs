// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Child output arrives as raw bytes in arbitrary chunks (`apprafter_core::Event::Output`);
//! the webview gets text. A chunk may end inside a multi-byte character, so the
//! undecodable tail is carried to the next chunk; bytes that can never be UTF-8 become
//! U+FFFD (Windows tools writing an OEM code page show up this way — D.3 walk checks it).

#[derive(Debug, Default)]
pub struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    /// Decode `bytes` after whatever was held back; returns the text that is complete.
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        let mut rest: &[u8] = &self.pending;
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    rest = &[];
                    break;
                }
                Err(e) => {
                    let (good, bad) = rest.split_at(e.valid_up_to());
                    out.push_str(std::str::from_utf8(good).expect("validated prefix"));
                    match e.error_len() {
                        Some(n) => {
                            out.push('\u{fffd}');
                            rest = &bad[n..];
                        }
                        None => {
                            rest = bad; // incomplete sequence at the end: keep it
                            break;
                        }
                    }
                }
            }
        }
        self.pending = rest.to_vec();
        out
    }

    /// End of stream: an incomplete tail can never complete.
    pub fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            String::new()
        } else {
            self.pending.clear();
            "\u{fffd}".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Utf8Stream;

    #[test]
    fn a_character_split_across_chunks_is_held_until_complete() {
        let mut d = Utf8Stream::default();
        let euro = "€".as_bytes(); // 3 bytes
        assert_eq!(d.push(&[b'a', euro[0]]), "a");
        assert_eq!(d.push(&euro[1..2]), "");
        assert_eq!(d.push(&[euro[2], b'b']), "€b");
    }

    #[test]
    fn an_invalid_byte_becomes_a_replacement_character_and_decoding_goes_on() {
        let mut d = Utf8Stream::default();
        assert_eq!(d.push(&[b'x', 0xff, b'y']), "x\u{fffd}y");
    }

    #[test]
    fn an_incomplete_tail_is_flushed_as_a_replacement_at_the_end() {
        let mut d = Utf8Stream::default();
        assert_eq!(d.push(&"€".as_bytes()[..2]), "");
        assert_eq!(d.finish(), "\u{fffd}");
        assert_eq!(d.finish(), "");
    }

    #[test]
    fn crlf_and_carriage_returns_pass_through_untouched() {
        let mut d = Utf8Stream::default();
        assert_eq!(d.push(b"50%\r60%\r\n"), "50%\r60%\r\n");
    }
}
