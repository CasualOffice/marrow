//! Plain text (T1). 449 files in the real corpus — M0 §6 priority 2.
//!
//! Also the T1 catch-all: it is registered last, so anything that decodes as
//! text and that no structured parser claimed still yields byte-and-line
//! provenance rather than dropping to metadata-only. That is the difference
//! between an extensionless `NOTES` file being searchable and being a name.

use std::ops::Range;

use marrow_core::{Code, Error, Result};

use crate::decode;
use crate::ir::{
    ArtifactBuilder, IrKind, IrNode, LineIndex, NodeAttrs, ParseOutcome, ParseWarning,
    ParsedArtifact, ParserTier,
};
use crate::parser::{ContentParser, FileProbe, ParseInput};

/// Extensions we refuse on sight. Everything else is offered to the decoder,
/// which has the only opinion that counts — the bytes.
///
/// This list is the app-internal noise and media M0 §2 actually found, plus the
/// container formats a later tier will own. It is a routing shortcut, not a
/// classification: FS-014 still holds, and a `.png` full of text would be
/// skipped here and picked up by nothing. That is the correct trade — the
/// alternative is decoding 3,478 JPEGs to discover they are JPEGs.
const NOT_TEXT: &[&str] = &[
    // images (35% of the corpus by count; M0 F6 — metadata is the whole story)
    "jpg", "jpeg", "png", "gif", "heic", "webp", "bmp", "tiff", "ico", "avif", //
    // fonts
    "ttf", "otf", "woff", "woff2", "eot", //
    // documents owned by T2/T3 when they exist
    "pdf", "docx", "xlsx", "pptx", "doc", "xls", "ppt", "odt", "ods", "epub", "rtf", //
    // archives and binaries
    "zip", "gz", "bz2", "xz", "zst", "tar", "7z", "rar", "dmg", "pkg", "so", "dylib", "a", "o",
    "exe", "dll", "class", "jar", "wasm", "bin", "db", "sqlite", "sqlite3", //
    // media
    "mp3", "wav", "flac", "aac", "m4a", "mp4", "mov", "avi", "mkv", "webm", //
    // app-internal noise M0 §6 recommends excluding outright
    "dat", "toc", "journal", "strings", "plist",
];

/// The T1 plain-text parser.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextParser;

impl TextParser {
    pub const ID: &'static str = "text";
    pub const VERSION: &'static str = "1";
}

impl ContentParser for TextParser {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn version(&self) -> &'static str {
        Self::VERSION
    }

    fn tier(&self) -> ParserTier {
        ParserTier::T1
    }

    fn handles(&self, probe: &FileProbe) -> bool {
        match probe.extension.as_deref() {
            Some(ext) => !NOT_TEXT.contains(&ext),
            // M0 found 97 extensionless files. `README`, `LICENSE`, `Makefile`
            // and `.gitignore` are all worth indexing and none of them has an
            // extension to route on.
            None => true,
        }
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedArtifact> {
        let decoded = decode::decode(input.bytes)?;
        let src = decoded.text.as_str();

        if src.trim().is_empty() {
            // Not an error about the system — a fact about the file. The router
            // turns this into a metadata-only artifact.
            return Err(Error::new(
                Code::ParLowYield,
                "This file has no textual content, so only its metadata is indexed.",
            ));
        }

        let mut b = ArtifactBuilder::new(Self::ID, Self::VERSION, self.tier(), input.budget);
        b.degrade_provenance(decoded.provenance_ceiling());

        let lines = LineIndex::new(src);
        let cap = b.budget().limits().max_node_text_bytes;
        let mut split_any = false;

        for block in blocks(src) {
            let parts = split_at_cap(src, block, cap);
            split_any |= parts.len() > 1;
            for part in parts {
                let node = IrNode::verbatim(IrKind::Paragraph, src, part.clone())?
                    .with_attrs(NodeAttrs::default().with_lines(&lines, &part));
                b.push(None, node)?;
            }
        }

        if split_any {
            b.warn(ParseWarning::new(
                Code::ParTruncated,
                "A single block of text was larger than the per-node budget and was split \
                 across nodes. Byte ranges stay exact; only the block boundaries are ours.",
            ));
            b.set_outcome(ParseOutcome::Partial);
        }

        if decoded.is_low_yield() {
            b.warn(ParseWarning::new(
                Code::ParLowYield,
                format!(
                    "{:.0}% of this file decoded to replacement characters, so it was probably \
                     not {}. The text is indexed as-is; re-save it as UTF-8 for a clean parse.",
                    decoded.replacement_ratio * 100.0,
                    decoded.encoding
                ),
            ));
            b.set_outcome(ParseOutcome::LowYield);
        }

        if b.node_count() == 0 {
            return Err(Error::new(
                Code::ParLowYield,
                "This file decoded to whitespace only, so only its metadata is indexed.",
            ));
        }

        Ok(b.finish())
    }
}

/// Blank-line-separated blocks, trimmed, as byte ranges into `src`.
///
/// Ranges rather than slices because a range is the provenance; a slice would
/// have to be located again later, and "again" is where offsets go wrong.
fn blocks(src: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut end = 0usize;
    let mut offset = 0usize;

    for line in src.split_inclusive('\n') {
        let blank = line.trim().is_empty();
        if blank {
            if let Some(s) = start.take() {
                out.push(s..end);
            }
        } else {
            if start.is_none() {
                let lead = line.len() - line.trim_start().len();
                start = Some(offset + lead);
            }
            end = offset + line.trim_end().len();
        }
        offset += line.len();
    }
    if let Some(s) = start {
        out.push(s..end);
    }
    out
}

/// Split a range so no piece exceeds `cap` bytes, preferring a line boundary
/// and never a codepoint boundary.
fn split_at_cap(src: &str, range: Range<usize>, cap: usize) -> Vec<Range<usize>> {
    if range.len() <= cap || cap == 0 {
        return vec![range];
    }
    let mut out = Vec::new();
    let mut start = range.start;
    while range.end - start > cap {
        // Back the hard limit off to a codepoint boundary *before* slicing;
        // `&src[a..b]` panics on a boundary violation, and a panic in a parser
        // is a bug the router should never have to catch.
        let mut hard = (start + cap).min(range.end);
        while hard > start && !src.is_char_boundary(hard) {
            hard -= 1;
        }
        if hard <= start {
            // One character is wider than the cap. Take it whole rather than
            // loop forever or split it.
            hard = start + 1;
            while hard < range.end && !src.is_char_boundary(hard) {
                hard += 1;
            }
        }
        // Prefer the last newline inside the window: a split on a line boundary
        // reads as a block, a split mid-word reads as corruption.
        let cut = src
            .get(start..hard)
            .and_then(|s| s.rfind('\n'))
            .map_or(hard, |i| start + i + 1);
        out.push(start..cut);
        start = cut;
    }
    if start < range.end {
        out.push(start..range.end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetGuard, Budgets};
    use marrow_core::{ProvenanceClass, SourceSpan};

    fn parse(src: &[u8], name: &str) -> Result<ParsedArtifact> {
        let probe = FileProbe::new(name, src.len() as u64);
        TextParser.parse(ParseInput {
            bytes: src,
            probe: &probe,
            budget: BudgetGuard::new(Budgets::default()),
        })
    }

    #[test]
    fn text_parser_gives_every_block_a_byte_and_line_span() {
        let src = "First block.\nStill first.\n\nSecond block.\n";
        let a = parse(src.as_bytes(), "notes.txt").unwrap();
        assert_eq!(a.nodes.len(), 2);
        assert_eq!(a.outcome, ParseOutcome::Ok);
        assert_eq!(a.provenance, ProvenanceClass::Exact);

        let first = &a.nodes[0];
        assert_eq!(first.kind, IrKind::Paragraph);
        assert_eq!(first.text(), Some("First block.\nStill first."));
        assert_eq!(first.span, SourceSpan::Bytes { start: 0, end: 25 });
        assert_eq!(first.attrs.line_start, Some(1));
        assert_eq!(first.attrs.line_end, Some(2));

        let second = &a.nodes[1];
        assert_eq!(second.text(), Some("Second block."));
        assert_eq!(second.attrs.line_start, Some(4));
        a.validate().unwrap();
    }

    #[test]
    fn an_empty_file_is_metadata_only_not_a_crash() {
        let e = parse(b"   \n\n\t\n", "empty.txt").unwrap_err();
        assert_eq!(e.code(), Code::ParLowYield);
        assert!(e.code().isolates_to_one_file());
    }

    #[test]
    fn binary_bytes_are_declined_so_the_chain_continues() {
        let e = parse(b"\x00\x01\x02binary", "thing.unknown").unwrap_err();
        assert_eq!(e.code(), Code::ParUnsupported);
    }

    #[test]
    fn known_binary_extensions_are_not_even_offered() {
        assert!(!TextParser.handles(&FileProbe::new("photo.JPEG", 1)));
        assert!(!TextParser.handles(&FileProbe::new("app.dat", 1)));
        assert!(TextParser.handles(&FileProbe::new("README", 1)));
        assert!(TextParser.handles(&FileProbe::new("notes.txt", 1)));
        assert!(TextParser.handles(&FileProbe::new("weird.qqq", 1)));
    }

    #[test]
    fn a_lossy_decode_reports_low_yield_and_degraded_provenance() {
        // Almost entirely bytes that are invalid UTF-8 and meaningless in any
        // single-byte encoding.
        let bytes: Vec<u8> = (0..200u16).map(|i| 0x80u8.wrapping_add(i as u8)).collect();
        let a = parse(&bytes, "mystery.txt");
        if let Ok(a) = a {
            assert_ne!(a.provenance, ProvenanceClass::Exact);
        }
        // Either outcome is acceptable; what must not happen is a panic or an
        // error that does not isolate to this one file.
    }

    #[test]
    fn blocks_are_trimmed_and_blank_lines_separate_them() {
        let src = "  a\n\n b \n c\n\n\n";
        assert_eq!(blocks(src), vec![2..3, 6..11]);
        assert_eq!(&src[6..11], "b \n c");
    }

    #[test]
    fn oversized_blocks_split_on_a_line_boundary_without_losing_bytes() {
        let src = "aaaa\nbbbb\ncccc\n";
        let parts = split_at_cap(src, 0..14, 6);
        assert_eq!(parts, vec![0..5, 5..10, 10..14]);
        let rejoined: String = parts.iter().map(|r| &src[r.clone()]).collect();
        assert_eq!(rejoined, &src[0..14]);
    }

    #[test]
    fn splitting_never_lands_mid_codepoint() {
        let src = "ééééééééé";
        let parts = split_at_cap(src, 0..src.len(), 5);
        for p in &parts {
            assert!(src.get(p.clone()).is_some(), "{p:?} must be a valid slice");
        }
    }
}
