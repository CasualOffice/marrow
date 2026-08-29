//! The content stage: parse → chunk → index.
//!
//! Attaches behind the hash stage. Kept separate from [`crate::pipeline`]
//! because it is the only part that reads file *contents*, and that is the part
//! with a budget, a panic boundary and an invariant to uphold.

use marrow_core::{ChunkId, FileId, Result, TierState, Timestamp, VersionId, WorkspaceId};
use marrow_index::TextDoc;
use marrow_parse::chunk::{chunk, ChunkPolicy};
use marrow_parse::ir::LineIndex;
use marrow_parse::parser::FileProbe;
use marrow_parse::router::ParserRouter;
use tracing::debug;

/// What the content stage needs about a file that the hash stage already knew.
#[derive(Clone, Debug)]
pub struct ContentInput {
    pub file_id: FileId,
    pub version_id: VersionId,
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub file_name: String,
    pub size: u64,
    pub tier: TierState,
    pub modified: Timestamp,
    pub origin: marrow_core::Origin,
}

/// What a parse produced, alongside the documents.
///
/// The parse record is not optional bookkeeping: PAR-003 makes the parser's
/// identity and version the mechanism by which an upgrade schedules
/// reprocessing. Chunks without it are searchable but unreprocessable.
#[derive(Debug)]
pub struct Extracted {
    pub docs: Vec<TextDoc>,
    pub parse: marrow_store::read::NewParse,
}

/// Parse and chunk one file into index documents.
///
/// Returns an empty vec — not an error — when there is nothing to index. A
/// binary with no parser is a fact about the file, not a failure (PAR-013), and
/// on this corpus that is most of it: 3,478 of the 41,110 files are photos.
pub fn documents_for(
    router: &ParserRouter,
    policy: &ChunkPolicy,
    input: &ContentInput,
    bytes: &[u8],
) -> Result<Extracted> {
    // **Invariant #5.** Reaching here with a non-Resident file would mean the
    // bytes were already read, which is the thing that triggers the download.
    // The caller must not have opened it; assert rather than trust.
    debug_assert!(
        input.tier.safe_to_read(),
        "content stage reached with a non-Resident file"
    );

    let probe = FileProbe::new(&input.file_name, input.size);
    let artifact = router.parse(bytes, &probe)?;
    let chunks = chunk(&artifact, policy);

    // Byte offsets are not something a human or an editor can jump to. This is
    // the last point where the source text is in hand, so it is the only cheap
    // place to resolve them — at query time we would have to re-read the file,
    // and it may have changed since.
    let lines = std::str::from_utf8(bytes).ok().map(LineIndex::new);

    // Recorded whether or not anything was chunked. A file with no parser is
    // the commonest outcome on this corpus — 3,478 photos — and "we looked and
    // there was nothing to extract" is exactly the fact that stops us looking
    // again every scan.
    let parse = marrow_store::read::NewParse {
        version_id: input.version_id,
        parser_id: artifact.parser_id.to_string(),
        parser_version: artifact.parser_version.to_string(),
        parser_tier: screaming_snake(&format!("{:?}", artifact.tier)),
        provenance_class: provenance_sql(artifact.provenance),
        outcome: screaming_snake(&format!("{:?}", artifact.outcome)),
        char_yield: Some(artifact.text_yield() as i64),
        page_count: None,
        // The column is CHECK-constrained to valid JSON. `format!("{:?}")`
        // produces Rust Debug output, which is not JSON and is rejected — and
        // only for files that actually warn, so 156 of 35,000 failed silently
        // into the error count while the rest wrote cleanly.
        warnings: warnings_json(&artifact.warnings),
        parsed_at: marrow_core::Timestamp::now(),
    };

    if chunks.is_empty() {
        debug!(path = %input.path, outcome = ?artifact.outcome, "no chunks");
        return Ok(Extracted {
            docs: Vec::new(),
            parse,
        });
    }

    let docs = chunks
        .into_iter()
        .map(|c| TextDoc {
            chunk_id: ChunkId::new(),
            file_id: input.file_id,
            version_id: input.version_id,
            workspace_id: input.workspace_id,
            path: input.path.clone(),
            title: c.context_prefix,
            body: c.text,
            span: to_line_span(&c.span, lines.as_ref()),
            provenance: c.provenance,
            // **Invariant #13.** Carried through so the query layer can bar
            // agent-written content from supporting a claim without having to
            // re-derive where it came from.
            origin: input.origin,
            modified: input.modified,
        })
        .collect();

    Ok(Extracted { docs, parse })
}

/// Warnings as a JSON array, or `None` when there are none.
///
/// Hand-rolling the escaping here would be the third bug in this function's
/// history; `serde_json` owns it.
fn warnings_json(warnings: &[marrow_parse::ir::ParseWarning]) -> Option<String> {
    if warnings.is_empty() {
        return None;
    }
    let as_text: Vec<String> = warnings.iter().map(|w| format!("{w:?}")).collect();
    serde_json::to_string(&as_text).ok()
}

/// `LowYield` → `LOW_YIELD`.
///
/// A plain `to_uppercase` gives `LOWYIELD`, which no CHECK constraint accepts —
/// and because the common outcomes are single words (`Ok`, `Partial`), a real
/// corpus run passes while the compound ones fail silently into the error
/// count. Underscores have to be inserted at the case boundaries.
fn screaming_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.extend(c.to_uppercase());
    }
    out
}

/// Enum name to the CHECK-constrained wire form.
fn provenance_sql(p: marrow_core::ProvenanceClass) -> String {
    use marrow_core::ProvenanceClass::*;
    match p {
        Exact => "EXACT",
        Degraded => "DEGRADED",
        Approximate => "APPROXIMATE",
        MetadataOnly => "METADATA_ONLY",
    }
    .to_string()
}

/// Convert a byte span to a line span where the source is available.
///
/// Keeps the byte span when it cannot: a span that is honest about being a byte
/// range beats one that names a line the file does not have.
fn to_line_span(
    span: &marrow_core::SourceSpan,
    lines: Option<&LineIndex>,
) -> marrow_core::SourceSpan {
    use marrow_core::SourceSpan::*;
    match (span, lines) {
        (Bytes { start, end }, Some(ix)) => Lines {
            start: ix.line_of(*start as usize),
            end: ix.line_of((*end as usize).saturating_sub(1).max(*start as usize)),
        },
        _ => span.clone(),
    }
}

/// Read a file's bytes, refusing anything that must not be read.
///
/// Separate from [`documents_for`] so the tier check sits at the point of the
/// open rather than one call away from it.
pub fn read_for_parsing(path: &str, tier: TierState, max_bytes: u64) -> Result<Option<Vec<u8>>> {
    if !tier.safe_to_read() {
        return Ok(None);
    }
    let meta = std::fs::metadata(path)?;
    if meta.len() > max_bytes {
        return Ok(None);
    }
    Ok(Some(std::fs::read(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_serialise_as_valid_json() {
        // The column is CHECK-constrained with json_valid(); Debug output is
        // not JSON, and the failure only appears for files that warn.
        use marrow_parse::ir::ParseWarning;
        assert_eq!(warnings_json(&[]), None);

        let json = warnings_json(&[
            ParseWarning::new(marrow_core::Code::ParLowYield, "little text extracted"),
            // Quotes and backslashes are exactly what hand-rolled escaping gets
            // wrong, so put them in the fixture.
            ParseWarning::new(marrow_core::Code::ParCorrupt, "bad \"token\" at C:\\x"),
        ])
        .expect("some warnings");

        let parsed: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 2);
    }

    #[test]
    fn compound_enum_names_get_their_underscores() {
        // The bug this pins: `to_uppercase` alone yields LOWYIELD, which the
        // CHECK constraint rejects — and only for the outcomes a synthetic
        // fixture produces, so a real corpus run looks fine.
        assert_eq!(screaming_snake("LowYield"), "LOW_YIELD");
        assert_eq!(screaming_snake("MetadataOnly"), "METADATA_ONLY");
        assert_eq!(screaming_snake("Ok"), "OK");
        assert_eq!(screaming_snake("T5"), "T5");
    }

    #[test]
    fn every_outcome_and_tier_satisfies_its_check_constraint() {
        // The schema's allowed sets, copied deliberately: if either side
        // changes, this fails rather than the write failing at runtime.
        const OUTCOMES: &[&str] = &[
            "OK",
            "PARTIAL",
            "LOW_YIELD",
            "FAILED",
            "UNSUPPORTED",
            "SKIPPED_POLICY",
            "METADATA_ONLY",
        ];
        const TIERS: &[&str] = &["T1", "T2", "T3", "T4", "T5"];

        for name in [
            "Ok",
            "Partial",
            "LowYield",
            "Failed",
            "Unsupported",
            "MetadataOnly",
        ] {
            let v = screaming_snake(name);
            assert!(
                OUTCOMES.contains(&v.as_str()),
                "{name} -> {v} is not a legal outcome"
            );
        }
        for name in ["T1", "T2", "T3", "T4", "T5"] {
            let v = screaming_snake(name);
            assert!(
                TIERS.contains(&v.as_str()),
                "{name} -> {v} is not a legal tier"
            );
        }
    }
}
