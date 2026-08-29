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
) -> Result<Vec<TextDoc>> {
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

    if chunks.is_empty() {
        debug!(path = %input.path, outcome = ?artifact.outcome, "no chunks");
        return Ok(Vec::new());
    }

    Ok(chunks
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
        .collect())
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
