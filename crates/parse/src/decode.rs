//! Bytes → text, and the honest reporting of what that cost.
//!
//! Shared by every text-ish parser in the crate, because every one of them has
//! the same three problems: is this even text, what encoding is it, and how
//! much did we lose.
//!
//! # Spans are offsets into the decoded text
//!
//! [`marrow_core::SourceSpan::Bytes`] says "byte range in the decoded content",
//! and that is what this crate records. For UTF-8 — effectively the whole real
//! corpus — decoded offsets *are* file offsets. For a legacy encoding they are
//! not, and pretending otherwise would make every citation on such a file
//! subtly wrong. [`Decoded::offsets_match_source`] says which case you are in,
//! so a caller that needs to seek in the original file knows whether it may.

use marrow_core::{Code, Error, ProvenanceClass, Result};

/// How much of the decoded text may be U+FFFD before we call the result
/// low-yield. One replacement character in a hundred is already a file we
/// decoded with the wrong label.
const REPLACEMENT_RATIO_LOW_YIELD: f32 = 0.01;

/// How far into a file we look for the NUL bytes that mean "binary".
///
/// Real text files do not contain NUL. Real binaries almost always do within
/// the first few KB — and the ones that do not (a JPEG, say) are caught by the
/// UTF-8 check instead.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Decoded text plus what the decode cost.
#[derive(Clone, Debug)]
pub struct Decoded {
    pub text: String,
    /// Encoding label actually used, e.g. `UTF-8`, `windows-1252`.
    pub encoding: &'static str,
    pub had_bom: bool,
    /// Fraction of characters that came out as U+FFFD.
    pub replacement_ratio: f32,
    /// True when the decode was lossless *and* byte offsets in `text` are byte
    /// offsets in the source file.
    pub offsets_match_source: bool,
}

impl Decoded {
    /// Whether the decode was clean enough to call the parse a success.
    pub fn is_low_yield(&self) -> bool {
        self.replacement_ratio > REPLACEMENT_RATIO_LOW_YIELD
    }

    /// The best provenance class a parse over this text can claim.
    ///
    /// Once offsets stop corresponding to the file, a byte range is a
    /// reconstruction rather than a fact, and CONV-003 has a word for that.
    pub fn provenance_ceiling(&self) -> ProvenanceClass {
        if self.offsets_match_source && !self.is_low_yield() {
            ProvenanceClass::Exact
        } else {
            ProvenanceClass::Degraded
        }
    }
}

/// Whether these bytes look like a binary blob rather than text.
pub fn looks_binary(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    // A BOM makes it text by declaration, even if a NUL follows (UTF-16).
    if encoding_rs::Encoding::for_bom(head).is_some() {
        return false;
    }
    head.contains(&0)
}

/// Decode `bytes` to text, or explain why they are not text.
///
/// Order of evidence, strongest first: a BOM, then valid UTF-8, then
/// `chardetng`'s guess. The guess is last because it is a guess — on a file
/// that is already valid UTF-8 it can still prefer a legacy label, and taking
/// it would corrupt the majority case to accommodate the minority.
pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    if looks_binary(bytes) {
        return Err(Error::new(
            Code::ParUnsupported,
            "This file contains NUL bytes, so it is not text. It stays discoverable through \
             its metadata; add a parser for its format to index the contents.",
        ));
    }

    if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, _, had_errors) = enc.decode(&bytes[bom_len..]);
        let text = text.into_owned();
        let ratio = replacement_ratio(&text);
        return Ok(Decoded {
            encoding: enc.name(),
            // A BOM shifts every offset even for UTF-8, so offsets only match
            // the file if we strip nothing.
            offsets_match_source: false,
            had_bom: true,
            replacement_ratio: if had_errors {
                ratio.max(f32::EPSILON)
            } else {
                ratio
            },
            text,
        });
    }

    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(Decoded {
            text: text.to_owned(),
            encoding: "UTF-8",
            had_bom: false,
            replacement_ratio: 0.0,
            offsets_match_source: true,
        });
    }

    let mut det = chardetng::EncodingDetector::new();
    det.feed(bytes, true);
    let enc = det.guess(None, true);
    let (text, _, _) = enc.decode(bytes);
    let text = text.into_owned();
    let replacement_ratio = replacement_ratio(&text);
    Ok(Decoded {
        encoding: enc.name(),
        had_bom: false,
        replacement_ratio,
        // Even when `chardetng` lands on UTF-8, we got here because the bytes
        // were *not* valid UTF-8, so something was replaced and offsets moved.
        offsets_match_source: false,
        text,
    })
}

fn replacement_ratio(text: &str) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let total = text.chars().count();
    let bad = text
        .chars()
        .filter(|c| *c == char::REPLACEMENT_CHARACTER)
        .count();
    bad as f32 / total as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_decodes_losslessly_and_keeps_source_offsets() {
        let d = decode("héllo\nwörld".as_bytes()).unwrap();
        assert_eq!(d.encoding, "UTF-8");
        assert!(d.offsets_match_source);
        assert_eq!(d.replacement_ratio, 0.0);
        assert_eq!(d.provenance_ceiling(), ProvenanceClass::Exact);
    }

    #[test]
    fn a_bom_is_stripped_and_disqualifies_source_offsets() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hi");
        let d = decode(&bytes).unwrap();
        assert!(d.had_bom);
        assert_eq!(d.text, "hi");
        assert!(!d.offsets_match_source);
        assert_eq!(d.provenance_ceiling(), ProvenanceClass::Degraded);
    }

    #[test]
    fn legacy_bytes_decode_and_are_flagged_degraded() {
        // 0xE9 is `é` in windows-1252 and invalid UTF-8.
        let d = decode(b"caf\xE9 au lait, r\xE9sum\xE9, na\xEFve").unwrap();
        assert!(d.text.contains("caf"));
        assert!(!d.offsets_match_source);
        assert_eq!(d.provenance_ceiling(), ProvenanceClass::Degraded);
    }

    #[test]
    fn nul_bytes_mean_not_text() {
        let e = decode(b"\x7fELF\x02\x01\x01\x00\x00\x00").unwrap_err();
        assert_eq!(e.code(), Code::ParUnsupported);
        assert!(looks_binary(b"PK\x03\x04\x00\x00"));
        assert!(!looks_binary(b"plain text"));
    }

    #[test]
    fn a_mojibake_ratio_reads_as_low_yield() {
        let d = Decoded {
            text: "a\u{FFFD}\u{FFFD}b".into(),
            encoding: "UTF-8",
            had_bom: false,
            replacement_ratio: 0.5,
            offsets_match_source: true,
        };
        assert!(d.is_low_yield());
        assert_eq!(d.provenance_ceiling(), ProvenanceClass::Degraded);
    }

    #[test]
    fn an_empty_file_decodes_to_empty_without_dividing_by_zero() {
        let d = decode(b"").unwrap();
        assert_eq!(d.text, "");
        assert_eq!(d.replacement_ratio, 0.0);
    }
}
