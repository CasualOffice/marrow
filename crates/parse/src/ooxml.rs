//! The OOXML container — a zip full of XML, and therefore hostile input.
//!
//! XLSX and DOCX are both zip archives. That single fact changes the threat
//! model for [`crate::budget`]: every other parser in this crate is handed the
//! bytes it will read, so `max_file_bytes` is a real bound on the work. A zip
//! is a *promise* about bytes that do not exist yet, and the promise is made by
//! the file. A 40 KB archive can declare a 4 GB part; a 4 GB part can be
//! declared as 40 KB and then keep inflating.
//!
//! So the work is bounded **before** it is done, the way [`crate::image`]
//! bounds on `MAX_PIXELS` rather than on the wall clock:
//!
//! | Ceiling | Why |
//! |---|---|
//! | [`MAX_ENTRIES`] | The central directory is attacker-sized. Enumerating it is itself work. |
//! | [`MAX_PART_BYTES`] | One part. `word/document.xml` above this is not a document. |
//! | [`MAX_TOTAL_UNCOMPRESSED`] | The sum of every declared size. This is the `42.zip` gate. |
//! | [`MAX_EXPANSION_RATIO`] | A small archive claiming a large expansion. Catches the bomb that stays under the absolute ceiling by being small. |
//!
//! **What the declared sizes cannot do**, stated rather than assumed: they are
//! the archive's claim about itself, and a crafted archive can declare a small
//! part and then inflate past it. That is why [`read_part`] reads through
//! `Read::take` and checks the bytes it actually got, instead of trusting
//! `size()` and allocating. For XLSX the inflation happens inside `calamine`,
//! which we do not control — there the declared-size gate is the whole defence,
//! and the residual risk is accepted knowingly rather than overlooked. The
//! per-sheet cell ceilings in [`crate::xlsx`] bound everything after that point.
//!
//! # Path traversal
//!
//! An entry named `../../.ssh/authorized_keys` is a real thing to find in a
//! hostile archive, and the reason it is harmless here is worth writing down:
//! **nothing in this crate ever materialises an entry to disk**, and parts are
//! fetched by exact name rather than enumerated and written. The traversal
//! attempt is still *reported* ([`Preflight::suspicious_names`]) rather than
//! ignored, because a document that contains one is a document worth flagging
//! even when it cannot hurt this particular reader.

use std::io::{Cursor, Read};

use marrow_core::{Code, Error, Result};
use zip::ZipArchive;

/// Entries we will look at in one archive.
///
/// A workbook with a chart, a theme and fifty sheets is a few hundred entries;
/// a document with images is fewer. Eight thousand is far above any real file
/// and far below the point where walking the directory costs anything.
pub const MAX_ENTRIES: usize = 8_192;

/// One decompressed part. `word/document.xml` for a 400-page report is a few
/// megabytes; 64 MB is not a document, it is a payload.
pub const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;

/// Every declared uncompressed size, summed. The `42.zip` gate.
pub const MAX_TOTAL_UNCOMPRESSED: u64 = 256 * 1024 * 1024;

/// Declared expansion over the archive's own size. A legitimate Office file
/// compresses XML 10–30×; 300 leaves room for a very repetitive spreadsheet and
/// still refuses a bomb that stayed under the absolute ceiling by being small.
pub const MAX_EXPANSION_RATIO: u64 = 300;

/// A local file header. Checked against the bytes rather than the extension,
/// per FS-014 — a `.xlsx` that is really a PDF must fall through the chain, not
/// die inside `calamine`.
pub fn looks_like_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

/// What [`preflight`] found. Carried back so the parser can warn rather than
/// having this module invent messages for two different formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preflight {
    pub entries: usize,
    /// Sum of the declared uncompressed sizes. A claim, not a measurement.
    pub declared_bytes: u64,
    /// Entries whose names escape the archive root. Harmless here; see the
    /// module docs.
    pub suspicious_names: usize,
}

/// Read the central directory and refuse the archive if it promises too much.
///
/// Runs before a single byte is inflated. That ordering is the point: checking
/// after decompression is not a budget, it is a post-mortem
/// ([`crate::budget`], rule 3).
pub fn preflight(bytes: &[u8]) -> Result<Preflight> {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(|e| {
        Error::new(
            Code::ParCorrupt,
            "This file is named as an Office document but its archive directory could not be \
             read. It may be truncated or damaged; it stays findable by name.",
        )
        .with_context(e.to_string())
    })?;

    if zip.len() > MAX_ENTRIES {
        return Err(budget(format!(
            "{} entries, ceiling {MAX_ENTRIES}",
            zip.len()
        )));
    }

    let mut declared: u64 = 0;
    let mut suspicious = 0usize;
    for i in 0..zip.len() {
        // `_raw`: opens the entry without starting a decompressor, so this loop
        // reads the directory and nothing else.
        let entry = zip.by_index_raw(i).map_err(|e| {
            Error::new(
                Code::ParCorrupt,
                "An entry in this Office document could not be read. The file may be damaged; \
                 it stays findable by name.",
            )
            .with_context(e.to_string())
        })?;
        if entry.enclosed_name().is_none() {
            suspicious += 1;
        }
        let size = entry.size();
        if size > MAX_PART_BYTES {
            return Err(budget(format!(
                "a part declares {size} bytes, ceiling {MAX_PART_BYTES}"
            )));
        }
        declared = declared.saturating_add(size);
        if declared > MAX_TOTAL_UNCOMPRESSED {
            return Err(budget(format!(
                "declared {declared} uncompressed bytes, ceiling {MAX_TOTAL_UNCOMPRESSED}"
            )));
        }
    }

    let compressed = bytes.len().max(1) as u64;
    if declared / compressed > MAX_EXPANSION_RATIO {
        return Err(budget(format!(
            "declares {declared} bytes from {compressed}, ratio ceiling {MAX_EXPANSION_RATIO}"
        )));
    }

    Ok(Preflight {
        entries: zip.len(),
        declared_bytes: declared,
        suspicious_names: suspicious,
    })
}

/// One part by exact name, or `None` when the archive does not have it.
///
/// `Read::take` rather than `with_capacity(entry.size())`: the declared size is
/// the archive's claim about itself and a crafted one lies in both directions.
/// Reading one byte past the ceiling and checking is the only version that
/// bounds a liar.
pub fn read_part(bytes: &[u8], name: &str) -> Result<Option<Vec<u8>>> {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(|e| {
        Error::new(
            Code::ParCorrupt,
            "This Office document's archive directory could not be read. It may be truncated \
             or damaged; it stays findable by name.",
        )
        .with_context(e.to_string())
    })?;
    let Ok(entry) = zip.by_name(name) else {
        return Ok(None);
    };

    let mut buf = Vec::new();
    entry
        .take(MAX_PART_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| {
            Error::new(
                Code::ParCorrupt,
                "A part of this Office document could not be decompressed. The file may be \
                 damaged; it stays findable by name.",
            )
            .with_context(e.to_string())
        })?;
    if buf.len() as u64 > MAX_PART_BYTES {
        return Err(budget(format!(
            "`{name}` inflated past {MAX_PART_BYTES} bytes despite its declared size"
        )));
    }
    Ok(Some(buf))
}

fn budget(context: String) -> Error {
    Error::new(
        Code::ParBudgetExceeded,
        "This Office document's internal parts are larger than the per-file parse budget \
         allows, so it is indexed by metadata only. That shape is also what a zip bomb looks \
         like; check the file if you did not expect it to be this large.",
    )
    .with_context(context)
}

#[cfg(test)]
pub(crate) mod test_zip {
    //! Building archives in tests.
    //!
    //! Stored, never deflated: a fixture's job is to be readable at a glance in
    //! the test that writes it, and compressing it would only exercise
    //! `flate2`. The bomb tests below are the exception and say so.

    use std::io::{Cursor, Write};

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    pub fn zip_of(parts: &[(&str, &[u8])]) -> Vec<u8> {
        build(parts, CompressionMethod::Stored)
    }

    pub fn deflated_zip_of(parts: &[(&str, &[u8])]) -> Vec<u8> {
        build(parts, CompressionMethod::Deflated)
    }

    fn build(parts: &[(&str, &[u8])], method: CompressionMethod) -> Vec<u8> {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(method);
        for (name, body) in parts {
            w.start_file(*name, opts).expect("start entry");
            w.write_all(body).expect("write entry");
        }
        w.finish().expect("finish archive").into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::test_zip::*;
    use super::*;

    #[test]
    fn a_zip_is_recognised_from_its_bytes_not_its_name() {
        // FS-014. A `.xlsx` full of something else must fall through the chain.
        assert!(looks_like_zip(&zip_of(&[("a.xml", b"<a/>")])));
        assert!(!looks_like_zip(b"%PDF-1.7"));
        assert!(!looks_like_zip(b""));
    }

    #[test]
    fn a_part_is_read_by_exact_name_and_a_missing_one_is_not_an_error() {
        let z = zip_of(&[("word/document.xml", b"<w:document/>")]);
        assert_eq!(
            read_part(&z, "word/document.xml").unwrap().as_deref(),
            Some(&b"<w:document/>"[..])
        );
        assert_eq!(read_part(&z, "word/footer1.xml").unwrap(), None);
    }

    #[test]
    fn a_traversing_entry_name_is_reported_rather_than_ignored() {
        // Harmless because nothing here writes an entry to disk — but a
        // document containing one is worth flagging.
        let z = zip_of(&[("../../etc/passwd", b"x"), ("word/document.xml", b"<a/>")]);
        let p = preflight(&z).unwrap();
        assert_eq!(p.suspicious_names, 1);
        assert_eq!(p.entries, 2);
    }

    #[test]
    fn a_declared_expansion_bomb_is_refused_before_anything_inflates() {
        // 8 MB of one repeated byte deflates to a few KB. The archive is honest
        // about its size here; the point is that the ratio gate fires on the
        // *directory*, without the parser ever holding 8 MB.
        let body = vec![b'A'; 8 * 1024 * 1024];
        let z = deflated_zip_of(&[("xl/sharedStrings.xml", &body)]);
        assert!(
            (z.len() as u64) < 64 * 1024,
            "fixture must actually compress: {} bytes",
            z.len()
        );
        let e = preflight(&z).unwrap_err();
        assert_eq!(e.code(), Code::ParBudgetExceeded);
        assert!(
            e.message().contains("zip bomb"),
            "SUP-001: name the cause — {}",
            e.message()
        );
    }

    #[test]
    fn an_ordinary_office_archive_passes_the_gate() {
        let z = zip_of(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("word/document.xml", b"<w:document/>"),
        ]);
        let p = preflight(&z).unwrap();
        assert_eq!(p.suspicious_names, 0);
        assert!(p.declared_bytes < 1024);
    }

    #[test]
    fn a_truncated_archive_isolates_to_one_file() {
        let mut z = zip_of(&[("word/document.xml", b"<a/>")]);
        z.truncate(z.len() / 2);
        let e = preflight(&z).unwrap_err();
        assert_eq!(e.code(), Code::ParCorrupt);
        assert!(e.code().isolates_to_one_file(), "the run must keep going");
    }
}
