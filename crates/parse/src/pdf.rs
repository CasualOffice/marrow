//! PDF, through PDFKit.
//!
//! # Why the OS and not pdfium
//!
//! The alternative is a multi-megabyte Chromium library to vendor, sign,
//! notarize and version-track — and the thing it would be doing is already on
//! every Mac. PDFKit gives, per page: the text, the media box, and
//! `characterBoundsAtIndex`, which is a rectangle **per character** in page
//! coordinates.
//!
//! That last one is the whole reason this file exists. Invariant #1 asks for a
//! span that resolves to an exact location, and `SourceSpan::Page { page, bbox }`
//! has existed since M1 with nothing producing it. A parser that could only say
//! "page 17" would satisfy the type and not the promise; this one can draw a
//! box around the sentence.
//!
//! # The index trap
//!
//! `characterBoundsAtIndex` indexes the page's `NSString`, which is **UTF-16**.
//! Rust byte offsets are UTF-8. For ASCII they agree and for everything else
//! they do not, so a naive `bounds[byte_offset]` is correct on the documents
//! you test with and wrong on the ones with an em dash in them. Every lookup
//! here goes through a UTF-8 → UTF-16 map built once per page.

use marrow_core::{Code, Error, ProvenanceClass, Result, SourceSpan};

use crate::ir::{ArtifactBuilder, IrKind, IrNode, NodeAttrs, ParseOutcome, ParsedArtifact};
use crate::parser::{ContentParser, FileProbe, ParseInput};
use crate::ParserTier;

/// A PDF's magic number. The extension is not the classifier (FS-014).
const MAGIC: &[u8] = b"%PDF-";

/// Pages beyond this are not read.
///
/// A 4,000-page scan would otherwise spend the whole budget on one file. The
/// artifact says it was truncated rather than pretending the document ends
/// here.
const MAX_PAGES: usize = 512;

#[derive(Debug, Default)]
pub struct PdfParser;

impl PdfParser {
    pub const ID: &'static str = "pdfkit";
    /// Bumped when output would change for the same input (invariant #4).
    pub const VERSION: &'static str = "1";
}

impl ContentParser for PdfParser {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn version(&self) -> &'static str {
        Self::VERSION
    }

    fn tier(&self) -> ParserTier {
        ParserTier::T2
    }

    fn handles(&self, probe: &FileProbe) -> bool {
        probe.extension.as_deref() == Some("pdf")
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedArtifact> {
        if !input.bytes.starts_with(MAGIC) {
            // The name said PDF and the bytes disagree. Handing it to the next
            // parser is the whole point of FS-014.
            return Err(Error::new(
                Code::ParUnsupported,
                "This file is named .pdf but does not start with a PDF header.",
            ));
        }
        backend::parse(self, input)
    }
}

/// Maps UTF-8 byte offsets in a page's text to UTF-16 offsets in the
/// `NSString` PDFKit indexes.
///
/// Built once per page. The alternative — converting per lookup — is O(n) each
/// time and this is called once per character of every node.
#[derive(Debug)]
pub(crate) struct Utf16Map {
    /// `utf16[i]` is the UTF-16 offset of the character starting at byte `i`.
    /// Bytes inside a character map to that character's start.
    utf16: Vec<u32>,
}

impl Utf16Map {
    pub(crate) fn new(text: &str) -> Self {
        let mut utf16 = vec![0u32; text.len() + 1];
        let mut u16_offset = 0u32;
        for (byte, ch) in text.char_indices() {
            for slot in utf16.iter_mut().skip(byte).take(ch.len_utf8()) {
                *slot = u16_offset;
            }
            u16_offset += ch.len_utf16() as u32;
        }
        if let Some(last) = utf16.last_mut() {
            *last = u16_offset;
        }
        Self { utf16 }
    }

    /// The UTF-16 index for a UTF-8 byte offset.
    pub(crate) fn at(&self, byte: usize) -> u32 {
        self.utf16
            .get(byte)
            .copied()
            .unwrap_or_else(|| self.utf16.last().copied().unwrap_or(0))
    }

    /// Total UTF-16 length.
    ///
    /// Pins the mapping in tests; the parser reaches the same value through
    /// `at(text.len())`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn len_utf16(&self) -> u32 {
        self.utf16.last().copied().unwrap_or(0)
    }
}

/// The union of a run of character rectangles, in PDF points.
///
/// `[x0, y0, x1, y1]` with the origin bottom-left, which is what PDF uses and
/// therefore what a viewer will expect to be handed back.
pub(crate) fn union(rects: impl Iterator<Item = [f32; 4]>) -> Option<[f32; 4]> {
    let mut out: Option<[f32; 4]> = None;
    for r in rects {
        // PDFKit returns an empty rect for a character with no glyph — a
        // newline, mostly. Including it would drag the box to the origin.
        if r[2] <= r[0] || r[3] <= r[1] {
            continue;
        }
        out = Some(match out {
            None => r,
            Some(o) => [
                o[0].min(r[0]),
                o[1].min(r[1]),
                o[2].max(r[2]),
                o[3].max(r[3]),
            ],
        });
    }
    out
}

/// Split a page's text into paragraphs, as byte ranges.
///
/// A blank line is the separator, which is what a PDF's extracted text gives
/// for a paragraph break. Runs of whitespace on their own are skipped: they
/// would become nodes with a box around nothing.
pub(crate) fn paragraphs(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = None;
    let mut blank_run = 0usize;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let _ = i;
        let offset = line.as_ptr() as usize - text.as_ptr() as usize;
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run >= 1 {
                if let Some(s) = start.take() {
                    out.push(s..offset);
                }
            }
        } else {
            blank_run = 0;
            start.get_or_insert(offset);
        }
    }
    if let Some(s) = start {
        out.push(s..text.len());
    }
    out.retain(|r| !text[r.clone()].trim().is_empty());
    out
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // Objective-C message sends. See the crate-level note.
mod backend {
    use super::*;
    use objc2::rc::Retained;
    use objc2::AllocAnyThread;

    use objc2_pdf_kit::{PDFDisplayBox, PDFDocument, PDFPage};

    /// PDFKit reads from a URL or from `NSData`.
    ///
    /// `NSData`, because the bytes are already in memory and a path would mean
    /// re-opening a file that invariant #5 has already resolved — and would
    /// mean this parser needed a path at all, which `ParseInput` deliberately
    /// does not carry (invariant #2).
    pub(super) fn parse(p: &PdfParser, input: ParseInput<'_>) -> Result<ParsedArtifact> {
        let data = objc2_foundation::NSData::with_bytes(input.bytes);
        let doc =
            unsafe { PDFDocument::initWithData(PDFDocument::alloc(), &data) }.ok_or_else(|| {
                Error::new(
                    Code::ParCorrupt,
                    "This PDF could not be opened. It may be damaged or encrypted; \
                     it stays findable by name.",
                )
            })?;

        if unsafe { doc.isEncrypted() } && unsafe { doc.isLocked() } {
            return Err(Error::new(
                Code::ParCorrupt,
                "This PDF is password-protected, so its text cannot be read. It \
                 stays findable by name.",
            ));
        }

        let pages = unsafe { doc.pageCount() };
        let mut b = ArtifactBuilder::new(PdfParser::ID, PdfParser::VERSION, p.tier(), input.budget);
        // The text is what PDFKit extracted, not what is on the page: ligature
        // and column handling are its business and we cannot verify them.
        // CONV-003 calls that Degraded, and a citation badge depends on it.
        b.degrade_provenance(ProvenanceClass::Degraded);

        let read = pages.min(MAX_PAGES);
        let mut with_text = 0usize;
        let mut chars_total = 0usize;

        for index in 0..read {
            let Some(page) = (unsafe { doc.pageAtIndex(index) }) else {
                continue;
            };
            let text = match unsafe { page.string() } {
                Some(s) => s.to_string(),
                None => continue,
            };
            if text.trim().is_empty() {
                continue;
            }
            with_text += 1;
            chars_total += text.chars().count();

            let map = Utf16Map::new(&text);
            let page_no = index as u32 + 1;

            for range in paragraphs(&text) {
                let bbox = bbox_for(&page, &map, &range);
                let node = IrNode::content(
                    IrKind::Paragraph,
                    SourceSpan::Page {
                        page: page_no,
                        bbox,
                    },
                    text[range].trim(),
                )
                .with_attrs(NodeAttrs::default());
                b.push(None, node)?;
            }
        }

        if pages > read {
            b.warn(crate::ir::ParseWarning::new(
                Code::ParTruncated,
                format!(
                    "This PDF has {pages} pages and the first {read} were read. The \
                     rest are not searchable."
                ),
            ));
            b.set_outcome(ParseOutcome::Partial);
        }

        // A PDF whose pages carry no text is a scan. Never silently dropped:
        // it is flagged so OCR can be offered (M0 F3).
        if with_text == 0 {
            return Err(Error::new(
                Code::ParLowYield,
                "No text could be extracted from this PDF, so it was probably \
                 scanned. It stays findable by name, and OCR would make its \
                 contents searchable.",
            ));
        }
        if chars_total < with_text * 32 {
            b.warn(crate::ir::ParseWarning::new(
                Code::ParLowYield,
                "This PDF yielded very little text for its page count, so it is \
                 probably a scan with a thin text layer. OCR would find more.",
            ));
            b.set_outcome(ParseOutcome::LowYield);
        }
        if b.node_count() == 0 {
            return Err(Error::new(
                Code::ParLowYield,
                "This PDF decoded to whitespace only, so only its metadata is indexed.",
            ));
        }

        Ok(b.finish())
    }

    /// The box around a byte range of a page's text.
    ///
    /// `None` when every character in the range is a newline or a space with no
    /// glyph — which is honest: there is nothing on the page to point at, and a
    /// box around the origin would be worse than no box.
    fn bbox_for(
        page: &Retained<PDFPage>,
        map: &Utf16Map,
        range: &std::ops::Range<usize>,
    ) -> Option<[f32; 4]> {
        let (from, to) = (map.at(range.start), map.at(range.end));
        if to <= from {
            return None;
        }
        // Bounded: a very long paragraph does not need every glyph measured to
        // know its box, and the loop is per character of every node.
        const MAX_PROBES: u32 = 4_000;
        let step = ((to - from) / MAX_PROBES).max(1);
        let media = unsafe { page.boundsForBox(PDFDisplayBox::MediaBox) };

        union((from..to).step_by(step as usize).filter_map(|i| {
            let r = unsafe { page.characterBoundsAtIndex(i as isize) };
            let (x, y, w, h) = (
                r.origin.x as f32,
                r.origin.y as f32,
                r.size.width as f32,
                r.size.height as f32,
            );
            // PDFKit occasionally returns a rect outside the page for a
            // character it could not place. A box off the page is not a
            // location, so it is dropped rather than unioned in.
            let out = [x, y, x + w, y + h];
            let (pw, ph) = (media.size.width as f32, media.size.height as f32);
            (out[0] >= -1.0 && out[1] >= -1.0 && out[2] <= pw + 1.0 && out[3] <= ph + 1.0)
                .then_some(out)
        }))
    }
}

/// Everywhere else, PDFs stay findable by name.
///
/// Not an error and not a silent drop: the router turns `ParUnsupported` into a
/// metadata-only record, which is what T5 promises.
#[cfg(not(target_os = "macos"))]
mod backend {
    use super::*;

    pub(super) fn parse(_p: &PdfParser, _input: ParseInput<'_>) -> Result<ParsedArtifact> {
        Err(Error::new(
            Code::ParUnsupported,
            "Reading PDFs uses PDFKit, which is only on macOS. This file stays \
             findable by name.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_named_pdf_that_is_not_one_is_handed_on() {
        // FS-014: the extension is not the classifier. Returning
        // `ParUnsupported` is what lets the router try the next parser.
        let probe = FileProbe::new("notes.pdf", 12);
        let e = PdfParser
            .parse(ParseInput {
                bytes: b"# actually markdown\n",
                probe: &probe,
                budget: crate::budget::BudgetGuard::new(crate::budget::Budgets::default()),
            })
            .unwrap_err();
        assert_eq!(e.code(), Code::ParUnsupported);
        assert!(
            PdfParser.handles(&probe),
            "it should still be routed here first"
        );
    }

    #[test]
    fn byte_offsets_map_to_utf16_indices() {
        // The trap this module is shaped around. PDFKit indexes an `NSString`,
        // which is UTF-16; Rust offsets are UTF-8. They agree on ASCII and
        // diverge on everything else, so a naive lookup is correct on the
        // documents you test with and wrong on the ones with an em dash.
        let text = "ab—cd";
        let m = Utf16Map::new(text);
        assert_eq!(m.at(0), 0);
        assert_eq!(m.at(1), 1);
        // The em dash is 3 bytes in UTF-8 and 1 unit in UTF-16.
        assert_eq!(m.at(2), 2);
        assert_eq!(m.at(5), 3, "after the dash, UTF-8 is 5 and UTF-16 is 3");
        assert_eq!(m.len_utf16(), 5);
    }

    #[test]
    fn an_astral_character_costs_two_utf16_units() {
        // An emoji is 4 bytes in UTF-8 and a surrogate *pair* in UTF-16.
        // Assuming one unit per character is the same bug one step further on.
        let m = Utf16Map::new("a🙂b");
        assert_eq!(m.at(0), 0);
        assert_eq!(m.at(1), 1);
        assert_eq!(m.at(5), 3, "the emoji occupies units 1 and 2");
        assert_eq!(m.len_utf16(), 4);
    }

    #[test]
    fn a_byte_offset_inside_a_character_maps_to_that_characters_start() {
        // Ranges come from paragraph splitting on `\n`, so they land on
        // boundaries — but a lookup that panicked or wrapped on an interior
        // byte would be a latent crash rather than a wrong box.
        let m = Utf16Map::new("a—b");
        assert_eq!(m.at(2), 1, "byte 2 is inside the dash");
        assert_eq!(m.at(3), 1);
        assert_eq!(m.at(999), m.len_utf16(), "past the end clamps");
    }

    #[test]
    fn a_union_ignores_rectangles_with_no_area() {
        // PDFKit returns an empty rect for a character with no glyph — a
        // newline, mostly. Including one drags the box to the origin, and a box
        // that starts at 0,0 points at the corner of the page.
        let boxes = [
            [10.0, 20.0, 30.0, 40.0],
            [0.0, 0.0, 0.0, 0.0],
            [5.0, 25.0, 35.0, 45.0],
        ];
        assert_eq!(
            union(boxes.into_iter()),
            Some([5.0, 20.0, 35.0, 45.0]),
            "the empty rect must not reach the origin"
        );
    }

    #[test]
    fn a_run_of_nothing_has_no_box_rather_than_a_box_at_the_origin() {
        // Honest: there is nothing on the page to point at. A citation to
        // (0,0) would be a location that is wrong rather than absent.
        assert_eq!(union(std::iter::empty()), None);
        assert_eq!(union([[0.0, 0.0, 0.0, 0.0]].into_iter()), None);
    }

    #[test]
    fn paragraphs_split_on_blank_lines_and_drop_the_blanks() {
        let text = "First para.\nStill first.\n\nSecond para.\n\n\nThird.\n";
        let parts: Vec<&str> = paragraphs(text)
            .iter()
            .map(|r| text[r.clone()].trim())
            .collect();
        assert_eq!(
            parts,
            vec!["First para.\nStill first.", "Second para.", "Third."]
        );
    }

    #[test]
    fn a_page_of_whitespace_yields_no_paragraphs() {
        // Otherwise every blank page becomes a node with a box around nothing.
        assert!(paragraphs("   \n\n \t\n").is_empty());
        assert!(paragraphs("").is_empty());
    }

    #[test]
    fn the_parser_identifies_itself_and_its_version() {
        // PAR-003: persisted with every result, so a version bump can schedule
        // reprocessing without a manual reindex (invariant #4).
        assert_eq!(PdfParser.id(), "pdfkit");
        assert!(!PdfParser.version().is_empty());
        assert_eq!(PdfParser.tier(), ParserTier::T2);
    }

    #[test]
    fn only_pdfs_are_routed_here() {
        assert!(PdfParser.handles(&FileProbe::new("a.pdf", 1)));
        assert!(!PdfParser.handles(&FileProbe::new("a.PDF.txt", 1)));
        assert!(!PdfParser.handles(&FileProbe::new("README", 1)));
    }
}

/// Against a real PDF. `#[ignore]` by default — it needs a file on disk.
///
/// `cargo test -p marrow-parse -- --ignored --nocapture pdf`
#[cfg(all(test, target_os = "macos"))]
mod real {
    use super::*;

    fn sample() -> Option<Vec<u8>> {
        let home = std::env::var_os("HOME")?;
        let dir = std::path::PathBuf::from(home).join("Downloads");
        std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
            let p = e.path();
            (p.extension()?.eq_ignore_ascii_case("pdf"))
                .then(|| std::fs::read(&p).ok())
                .flatten()
        })
    }

    #[test]
    #[ignore = "needs a real PDF in ~/Downloads"]
    fn a_real_pdf_yields_paragraphs_with_a_page_and_a_box() {
        // Invariant #1, finally produced rather than merely typed:
        // `SourceSpan::Page { page, bbox }` has existed since M1 with nothing
        // emitting it.
        let Some(bytes) = sample() else {
            panic!("put a PDF in ~/Downloads to run this");
        };
        let probe = FileProbe::new("sample.pdf", bytes.len() as u64);
        let art = PdfParser
            .parse(ParseInput {
                bytes: &bytes,
                probe: &probe,
                budget: crate::budget::BudgetGuard::new(crate::budget::Budgets::default()),
            })
            .expect("the PDF should parse");

        assert!(!art.nodes.is_empty(), "no nodes came out");
        assert_eq!(
            art.provenance,
            ProvenanceClass::Degraded,
            "extraction is not quotation"
        );

        let mut with_box = 0usize;
        for n in art.nodes.iter().take(400) {
            let SourceSpan::Page { page, bbox } = &n.span else {
                panic!("a PDF node must carry a page span, got {:?}", n.span);
            };
            assert!(*page >= 1, "pages are 1-based");
            if let Some(b) = bbox {
                with_box += 1;
                assert!(b[2] > b[0] && b[3] > b[1], "a box must have area: {b:?}");
            }
        }
        assert!(
            with_box * 2 > art.nodes.len().min(400),
            "most nodes should have a box; only {with_box} of {} did",
            art.nodes.len().min(400)
        );

        let first = art.nodes.iter().find(|n| n.text().is_some()).unwrap();
        eprintln!(
            "\n  {} nodes, {} with a box\n  first: {:?}\n  text: {:?}\n",
            art.nodes.len(),
            with_box,
            first.span,
            first.text().unwrap().chars().take(70).collect::<String>()
        );
    }
}
