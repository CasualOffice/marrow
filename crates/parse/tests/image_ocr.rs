//! The image parser against real pixels and the real recogniser.
//!
//! # Why the fixture is generated rather than committed
//!
//! A checked-in PNG is an opaque blob: nobody can tell from a diff what text it
//! contains, so the assertion below and the file it asserts against drift apart
//! silently. Here the expected text is a string literal three lines above the
//! assertion, and the image is drawn from it. It is also a rule of this repo
//! not to commit binary artefacts.
//!
//! # Why a hand-drawn bitmap font
//!
//! Rendering with Core Text would need two more Objective-C frameworks and a
//! second block of `unsafe` that exists only for tests. Shelling out to
//! `cupsfilter` and `sips` works — it is how the parser was first proved
//! against real antialiased Helvetica — but a test that silently passes when a
//! system tool is missing is worse than no test. A 5x7 font scaled up is a few
//! hundred bytes of table, needs nothing at all, and produces the same bytes on
//! every machine forever.
//!
//! Two things about this font are worth knowing before changing the fixture
//! text, because both were found the hard way:
//!
//! - **The scale factor is not arbitrary.** At 8 the recogniser finds nothing
//!   at all and at 16 it starts confusing letters. 24 is the first size that
//!   reads cleanly, which is itself useful to have written down: the floor on
//!   real screenshots is roughly a 24-pixel cap height.
//! - **Five columns is not enough to draw every letter distinctly.** `W` reads
//!   as `N` and `A` reads as `R` in some words, because at 5x7 they genuinely
//!   are nearly the same shape. That is a limit of the fixture, not of the
//!   parser — it reads real antialiased Helvetica without trouble. The fixture
//!   strings below were chosen from glyphs that survive the round trip, so a
//!   new one needs checking rather than assuming.

#![allow(clippy::needless_range_loop)] // The x/y loops below read as coordinates.

use marrow_core::{ProvenanceClass, SourceSpan};
use marrow_parse::{parse, FileProbe, ParseOutcome, ParserTier};

/// Cap height in source pixels per glyph row. See the module note.
const SCALE: usize = 24;

// ---------------------------------------------------------------------------
// A 5x7 bitmap font, one `u8` per row, bit 4 leftmost.
// ---------------------------------------------------------------------------

#[rustfmt::skip] // One glyph per line, five bits wide — the shape is the point.
const FONT: &[(char, [u8; 7])] = &[
    (' ', [0, 0, 0, 0, 0, 0, 0]),
    ('A', [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
    ('B', [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110]),
    ('C', [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110]),
    ('D', [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100]),
    ('E', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111]),
    ('F', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('G', [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111]),
    ('H', [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
    ('I', [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('J', [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100]),
    ('K', [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001]),
    ('L', [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111]),
    ('M', [0b10001, 0b11011, 0b10101, 0b10001, 0b10001, 0b10001, 0b10001]),
    ('N', [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001]),
    ('O', [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('P', [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('Q', [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101]),
    ('R', [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001]),
    ('S', [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110]),
    ('T', [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100]),
    ('U', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('V', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100]),
    ('W', [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001]),
    ('X', [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001]),
    ('Y', [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100]),
    ('Z', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111]),
    ('0', [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110]),
    ('1', [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('2', [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111]),
    ('3', [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110]),
    ('4', [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010]),
    ('5', [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110]),
    ('6', [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110]),
    ('7', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000]),
    ('8', [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110]),
    ('9', [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100]),
];

/// A grid of ink, drawn before it is encoded.
struct Canvas {
    width: usize,
    height: usize,
    /// One byte per pixel, 255 white. Grey rather than colour: the BMP encoder
    /// widens it, and nothing here needs a hue.
    ink: Vec<u8>,
}

impl Canvas {
    /// Draw `text` — one line per `\n` — with a generous white margin.
    ///
    /// The margin matters. Vision is noticeably worse at text that touches the
    /// edge of the frame, and a fixture that fails for that reason would look
    /// like a parser bug.
    fn draw(text: &str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let margin = 12 * SCALE;
        // 6 columns per glyph: five of face and one of tracking. 11 rows per
        // line: seven of face and four of leading.
        let width = cols * 6 * SCALE + 2 * margin;
        let height = lines.len() * 11 * SCALE + 2 * margin;
        let mut c = Canvas {
            width,
            height,
            ink: vec![255u8; width * height],
        };
        for (row, line) in lines.iter().enumerate() {
            for (col, ch) in line.chars().enumerate() {
                c.glyph(ch, margin + col * 6 * SCALE, margin + row * 11 * SCALE);
            }
        }
        c
    }

    fn glyph(&mut self, ch: char, left: usize, top: usize) {
        let upper = ch.to_ascii_uppercase();
        let rows = FONT
            .iter()
            .find(|(c, _)| *c == upper)
            .map(|(_, g)| *g)
            .unwrap_or_else(|| panic!("the fixture font has no glyph for {ch:?}"));
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 {
                    continue;
                }
                for dy in 0..SCALE {
                    for dx in 0..SCALE {
                        let x = left + col * SCALE + dx;
                        let y = top + row * SCALE + dy;
                        self.ink[y * self.width + x] = 0;
                    }
                }
            }
        }
    }

    /// Encode as an uncompressed 24-bit BMP.
    ///
    /// BMP because it is the one format in the parser's list that can be
    /// written correctly in thirty lines with no compression, no checksum and
    /// no dependency. The parser sniffs it like any other image, so nothing
    /// about the path under test is special-cased for it.
    fn to_bmp(&self) -> Vec<u8> {
        // Rows are padded to a four-byte boundary and stored bottom-up.
        let stride = (self.width * 3 + 3) & !3;
        let pixels = stride * self.height;
        let mut out = Vec::with_capacity(54 + pixels);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(54u32 + pixels as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // two reserved u16s
        out.extend_from_slice(&54u32.to_le_bytes()); // offset to pixel data
        out.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER size
        out.extend_from_slice(&(self.width as i32).to_le_bytes());
        out.extend_from_slice(&(self.height as i32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB, no compression
        out.extend_from_slice(&(pixels as u32).to_le_bytes());
        out.extend_from_slice(&2835i32.to_le_bytes()); // 72 dpi, in px/metre
        out.extend_from_slice(&2835i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // palette: none
        out.extend_from_slice(&0u32.to_le_bytes());
        for y in (0..self.height).rev() {
            let row = &self.ink[y * self.width..(y + 1) * self.width];
            for v in row {
                out.extend_from_slice(&[*v, *v, *v]); // BGR, and grey is grey
            }
            out.resize(out.len() + stride - self.width * 3, 0);
        }
        out
    }

    /// Encode as a PNG whose background is fully transparent and whose ink is
    /// opaque black — a diagram exported from Excalidraw, Figma or draw.io,
    /// which all default to a transparent background.
    ///
    /// The deflate stream uses stored (uncompressed) blocks. They are legal
    /// deflate, every decoder accepts them, and they mean this file needs no
    /// compression library to write — only a CRC-32 and an Adler-32, which are
    /// six lines each.
    fn to_transparent_png(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity(self.height * (1 + self.width * 4));
        for y in 0..self.height {
            raw.push(0u8); // filter type 0: none
            for x in 0..self.width {
                // Ink is opaque black, background is transparent black. Both
                // have RGB (0,0,0), so only the alpha distinguishes them —
                // which is exactly the shape that reads as black-on-black.
                let alpha = 255 - self.ink[y * self.width + x];
                raw.extend_from_slice(&[0, 0, 0, alpha]);
            }
        }

        let mut zlib = vec![0x78, 0x01]; // deflate, 32K window, no preset dict
        for (i, block) in raw.chunks(0xffff).enumerate() {
            let last = (i + 1) * 0xffff >= raw.len();
            zlib.push(u8::from(last));
            zlib.extend_from_slice(&(block.len() as u16).to_le_bytes());
            zlib.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            zlib.extend_from_slice(block);
        }
        zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&(self.width as u32).to_be_bytes());
        ihdr.extend_from_slice(&(self.height as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace

        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &zlib);
        chunk(&mut out, b"IEND", &[]);
        out
    }
}

/// Append one length-prefixed, CRC-checked PNG chunk.
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            // The reflected CRC-32 polynomial, computed rather than tabulated:
            // this runs once per fixture and a 256-entry table would be more
            // code than the loop it replaces.
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn image_of(text: &str) -> Vec<u8> {
    Canvas::draw(text).to_bmp()
}

/// The same text on a transparent background instead of a white one.
fn transparent_image_of(text: &str) -> Vec<u8> {
    Canvas::draw(text).to_transparent_png()
}

/// A blank sheet: correctly formed, with nothing written on it.
fn blank_image() -> Vec<u8> {
    Canvas {
        width: 640,
        height: 480,
        ink: vec![255u8; 640 * 480],
    }
    .to_bmp()
}

#[test]
fn the_generated_fixture_is_a_well_formed_bmp() {
    // If this fails, every assertion below is testing the encoder rather than
    // the parser, so it is worth separating.
    let bytes = image_of("AB");
    assert_eq!(&bytes[..2], b"BM");
    let width = i32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
    let height = i32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
    assert_eq!(width as usize, 2 * 6 * SCALE + 24 * SCALE);
    assert_eq!(height as usize, 11 * SCALE + 24 * SCALE);
    assert_eq!(bytes.len(), 54 + (width as usize * 3) * height as usize);
}

#[test]
fn a_file_named_png_that_is_not_an_image_is_handed_on_rather_than_failed() {
    // FS-014: the extension is not the classifier. These bytes are Markdown, so
    // the image parser must decline with `ParUnsupported` and let the chain
    // continue.
    //
    // The chain then runs out — `markdown` routes on the `.md` extension and
    // `text` refuses every image extension outright — so the file lands on the
    // metadata-only terminal, which is the correct T5 answer for it. The load
    // bearing assertion is the *silence*: `ParUnsupported` is the one failure
    // the router does not record, so an empty warning list is the difference
    // between "handed on" and "failed".
    let markdown = b"# Not a diagram\n\nJust text that was misnamed.\n";
    let probe = FileProbe::new("diagram.png", markdown.len() as u64);
    let artifact = parse(markdown, &probe).expect("routing never fails on content");
    assert_eq!(artifact.outcome, ParseOutcome::MetadataOnly);
    assert!(
        artifact.warnings.is_empty(),
        "declining a wrong extension is the chain working, not a problem: {:?}",
        artifact.warnings
    );
}

/// Everything below needs the Vision framework, so it needs a Mac.
#[cfg(target_os = "macos")]
mod vision {
    use super::*;

    #[test]
    fn text_in_an_image_is_extracted_with_a_page_span_and_a_box() {
        // The `source_span` rule, on the tier where it is hardest to honour: nothing in
        // these bytes is text, so the span has to be reconstructed from where
        // the recogniser saw ink.
        //
        // Vision splits this into one observation per word, which is why the
        // comparison rejoins them. Where it splits is its business — the
        // fixture asserts what was read and where, not how it was grouped.
        const TEXT: &str = "PARSE INDEX";
        let bytes = image_of(TEXT);
        let probe = FileProbe::new("screenshot.bmp", bytes.len() as u64);
        let artifact = parse(&bytes, &probe).expect("an image parse never fails the run");

        assert_eq!(artifact.parser_id, "vision-ocr");
        assert_eq!(artifact.tier, ParserTier::T4);
        assert_eq!(
            artifact.provenance,
            ProvenanceClass::Approximate,
            "OCR is reconstruction; a citation from it must carry the badge"
        );
        assert_eq!(artifact.outcome, ParseOutcome::Ok);
        artifact.validate().expect("the router validates this too");

        let read: String = artifact
            .nodes
            .iter()
            .filter_map(|n| n.text())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            read, TEXT,
            "the recogniser should read the fixture exactly at this size"
        );

        let node = &artifact.nodes[0];
        assert_eq!(
            node.attrs.confidence.map(|c| c > 0.5),
            Some(true),
            "a clean synthetic image should read confidently, got {:?}",
            node.attrs.confidence
        );
        let SourceSpan::Page { page, bbox } = &node.span else {
            panic!("an image node must carry a page span, got {:?}", node.span);
        };
        assert_eq!(*page, 1, "an image is one page by definition");
        assert!(bbox.is_some(), "the line was placed, so it has a box");
    }

    #[test]
    fn the_box_lands_on_the_region_the_text_was_drawn_in() {
        // A span that merely type-checks is not provenance. This pins the box
        // to where the fixture actually put the ink, in the image's own pixels
        // with the origin at the bottom left — the convention `pdf.rs` uses.
        let bytes = image_of("INDEX");
        let probe = FileProbe::new("diagram.bmp", bytes.len() as u64);
        let artifact = parse(&bytes, &probe).expect("an image parse never fails the run");

        let width = (5 * 6 * SCALE + 24 * SCALE) as f32;
        let height = (11 * SCALE + 24 * SCALE) as f32;
        let margin = (12 * SCALE) as f32;

        let SourceSpan::Page { bbox: Some(b), .. } = &artifact.nodes[0].span else {
            panic!(
                "expected a placed page span, got {:?}",
                artifact.nodes[0].span
            );
        };
        assert!(
            b[0] >= 0.0 && b[1] >= 0.0 && b[2] <= width && b[3] <= height,
            "the box must be inside the image {width}x{height}: {b:?}"
        );
        assert!(b[2] > b[0] && b[3] > b[1], "a box must have area: {b:?}");

        // The glyphs start one margin in from the left and run to one margin
        // from the right. A tenth of the image is ample slack for the
        // recogniser's own idea of where a letter ends.
        let slack = width / 10.0;
        assert!(
            (b[0] - margin).abs() < slack,
            "left edge {} should be near the margin {margin}",
            b[0]
        );
        assert!(
            (b[2] - (width - margin)).abs() < slack,
            "right edge {} should be near {}",
            b[2],
            width - margin
        );
        // Drawn on the first of one text row, so the box straddles the middle.
        let centre = (b[1] + b[3]) / 2.0;
        assert!(
            centre > height * 0.3 && centre < height * 0.7,
            "vertical centre {centre} should be mid-image in a {height}-tall frame"
        );
    }

    #[test]
    fn several_lines_come_back_in_reading_order() {
        // Document order is what the chunker and every "next citation" walk
        // depend on. Vision returns observations in detection order, which is
        // not the same thing.
        let bytes = image_of("FIRST LINE\nSECOND LINE\nTHIRD LINE");
        let probe = FileProbe::new("whiteboard.bmp", bytes.len() as u64);
        let artifact = parse(&bytes, &probe).expect("an image parse never fails the run");

        let lines: Vec<&str> = artifact.nodes.iter().filter_map(|n| n.text()).collect();
        assert_eq!(lines, vec!["FIRST LINE", "SECOND LINE", "THIRD LINE"]);

        // And the boxes descend, which is what "reading order" means when the
        // origin is at the bottom.
        let tops: Vec<f32> = artifact
            .nodes
            .iter()
            .filter_map(|n| match &n.span {
                SourceSpan::Page { bbox: Some(b), .. } => Some(b[3]),
                _ => None,
            })
            .collect();
        assert_eq!(tops.len(), 3);
        assert!(
            tops[0] > tops[1] && tops[1] > tops[2],
            "boxes should descend down the image: {tops:?}"
        );
    }

    #[test]
    fn dark_text_on_a_transparent_background_is_still_read() {
        // The gap this test exists for: Vision composites a transparent image
        // onto black, so a diagram exported with a transparent background and
        // dark ink comes back with *no observations at all* — which is
        // indistinguishable from "this image has no text in it". A silent
        // evidence gap in one of the three cases the parser was built for.
        //
        // The same fixture in the same size and position, on white, is the
        // subject of the tests above; the only difference here is the alpha.
        let bytes = transparent_image_of("PARSE INDEX");
        assert_eq!(
            &bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "the fixture must be a real PNG"
        );

        let probe = FileProbe::new("exported.png", bytes.len() as u64);
        let artifact = parse(&bytes, &probe).expect("an image parse never fails the run");

        assert_eq!(artifact.parser_id, "vision-ocr", "it must not fall through");
        let read: String = artifact
            .nodes
            .iter()
            .filter_map(|n| n.text())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(read, "PARSE INDEX");

        // And the retry must not have cost the provenance: the box is measured
        // against the composited image, which has the original's extent.
        let SourceSpan::Page { bbox: Some(b), .. } = &artifact.nodes[0].span else {
            panic!(
                "expected a placed page span, got {:?}",
                artifact.nodes[0].span
            );
        };
        assert!(b[2] > b[0] && b[3] > b[1], "a box must have area: {b:?}");
    }

    #[test]
    fn an_image_with_no_text_is_metadata_only_rather_than_an_error() {
        // The common case: most images are photographs. "A file with no parser
        // stays discoverable via metadata (T5). Not a failure."
        let bytes = blank_image();
        let probe = FileProbe::new("sunset.bmp", bytes.len() as u64);
        let artifact = parse(&bytes, &probe).expect("no text is not an error");

        assert_eq!(artifact.outcome, ParseOutcome::MetadataOnly);
        assert_eq!(artifact.tier, ParserTier::T5);
        assert_eq!(artifact.provenance, ProvenanceClass::MetadataOnly);
        assert_eq!(artifact.nodes.len(), 1);
        assert_eq!(artifact.nodes[0].span, SourceSpan::Whole);
        // The reason survives into the index-health view rather than only into
        // a log line, and it says what still works.
        let warning = &artifact.warnings[0];
        assert_eq!(warning.code, "PAR_LOW_YIELD");
        assert!(warning.message.contains("findable"), "{}", warning.message);
    }
}
