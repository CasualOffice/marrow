//! The `FI` file-intelligence read model ([Part 5 §99.1], [LLD §2.7]).
//!
//! Everything the system knows about one file, in one place. **FI-005: it is a
//! read model, never a separate store.** [`file_intelligence`] assembles it on
//! demand inside one read transaction — one snapshot, no cache to invalidate,
//! nothing that can go stale between sections. At M0's 9.4k files that is
//! single-digit milliseconds.
//!
//! ## What M1 can and cannot answer
//!
//! §99.1 lists thirteen sections. The M1 schema (eleven tables) can answer five
//! of them. The other eight are **[`UnansweredSection`]s**: the field is an
//! `Option` that is `None`, and the reason is carried alongside it.
//!
//! That distinction is FI-003 — *unknown is shown as unknown* — and it is the
//! whole reason this module does not simply omit those fields. An empty vector
//! renders as "we looked and there was nothing"; `None` plus a reason renders
//! as "not extracted yet", which is a different fact and the true one. UX §5
//! makes the same point about `—` versus a blank cell.
//!
//! | §99.1 section | M1 |
//! |---|---|
//! | Identity | [`Identity`] — id, hash, size, MIME, duplicates (FS-008) |
//! | Location | [`FileLocation`] — workspace, root, tier, **path history** (FS-006) |
//! | Versions | [`Versions`] — count, current, supersedes chain |
//! | Text extraction health | [`IndexState`] — chunks, provenance, parse result, jobs |
//! | Chunks | [`ChunkSummary`] — counts and shape, never bodies |
//! | Embedded metadata (§69) | `None` — no `file_metadata` table in M1 |
//! | Structure (IR outline) | `None` — `ir_nodes` exists but nothing writes it |
//! | Tables, media, entities, timeline, links, actions | `None` — M3/M4/M5 |
//!
//! [Part 5 §99.1]: ../../../docs/Part_5_Capabilities.md
//! [LLD §2.7]: ../../../docs/LLD.md

use marrow_core::{
    Code, ContentHash, Error, FileId, FileStatus, JobId, Origin, ParseId, ProvenanceClass, Result,
    RootId, SourceSpan, TierState, Timestamp, VersionId, VersionStatus, WorkspaceId,
};
use marrow_store::read::{self, FileRow, VersionRow};
use marrow_store::rusqlite::{params, Connection, OptionalExtension};
use marrow_store::{map_sqlite, Store};
use serde::Serialize;

use crate::search::{relative_path, workspace_of};

/// How to name the file to look up.
///
/// A path is a **lookup**, never an identity (invariant #2). Resolving one
/// yields a [`FileId`], and everything after that is keyed on the id — which is
/// why [`FileRef::Path`] and [`FileRef::Id`] produce identical panels for the
/// same file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileRef {
    Id(FileId),
    Path(String),
}

impl From<FileId> for FileRef {
    fn from(id: FileId) -> Self {
        FileRef::Id(id)
    }
}

// ------------------------------------------------------------------ the panel

/// The `FI` panel for one file.
#[derive(Clone, Debug, Serialize)]
pub struct FileIntelligence {
    pub identity: Identity,
    pub location: FileLocation,
    pub versions: Versions,
    pub index_state: IndexState,
    pub chunks: ChunkSummary,

    /// §99.1 "What this file told us about itself" — EXIF, XMP, IPTC, OOXML
    /// core/app properties, PDF info, ID3, xattr download origin (§69).
    ///
    /// **Always `None` in M1.** §106's metadata tables are not in the M1 schema
    /// staging, so nothing extracts or stores this. FI-003: `None` means "not
    /// extracted", which is not the same fact as an empty list.
    pub embedded_metadata: Option<Vec<MetadataField>>,

    /// §99.1 Structure — the IR outline: headings, sheets, slides, symbols.
    ///
    /// **Always `None` in M1.** `ir_nodes` is in the schema, but the M1 ingest
    /// path parses straight to chunks and never writes a node, so there is no
    /// outline to read. Reporting an empty outline for a document that
    /// obviously has headings would be a lie the panel tells itself.
    pub structure: Option<Vec<OutlineEntry>>,

    /// §99.1 Entities & relations, each with an authority class (invariant #14).
    ///
    /// **Always `None`.** The graph tables are deliberately absent from M1.
    pub entities: Option<Vec<EntityMention>>,

    /// §99.1 Timeline — created/modified/renamed events, Git history, agent
    /// actions.
    ///
    /// **Always `None` in M1.** The two event streams M1 *can* reconstruct are
    /// already exposed truthfully as [`Versions::history`] and
    /// [`FileLocation::path_history`]; synthesizing a "timeline" from those two
    /// alone would imply a completeness that is not there.
    pub timeline: Option<Vec<TimelineEvent>>,
}

impl FileIntelligence {
    /// The §99.1 sections this build cannot answer, and why.
    ///
    /// FI-003 in one call: a renderer walks this to print `—  not extracted
    /// yet` for each, instead of leaving a section silently blank.
    pub fn unanswered_sections(&self) -> Vec<UnansweredSection> {
        let mut out = Vec::new();
        if self.embedded_metadata.is_none() {
            out.push(UnansweredSection {
                section: "What this file says about itself",
                reason: "No metadata extractor runs yet; embedded EXIF/XMP/OOXML/ID3 properties \
                         are not stored.",
            });
        }
        if self.structure.is_none() {
            out.push(UnansweredSection {
                section: "Structure",
                reason: "The parser writes chunks directly; no IR outline is persisted yet.",
            });
        }
        if self.entities.is_none() {
            out.push(UnansweredSection {
                section: "Entities & relations",
                reason: "The knowledge graph is not part of this build.",
            });
        }
        if self.timeline.is_none() {
            out.push(UnansweredSection {
                section: "Timeline",
                reason: "Version and path history are shown in their own sections; no unified \
                         event log exists yet.",
            });
        }
        // Sections with no field at all: nothing in the M1 schema could carry
        // them, so there is no `Option` to be `None`. They are named here so
        // the panel still accounts for every row of §99.1.
        out.push(UnansweredSection {
            section: "Tables",
            reason: "Table IR is not part of this build.",
        });
        out.push(UnansweredSection {
            section: "Media derivatives",
            reason: "OCR, transcripts and captions are not part of this build.",
        });
        out.push(UnansweredSection {
            section: "Links",
            reason: "Inbound and outbound link extraction is not part of this build.",
        });
        out.push(UnansweredSection {
            section: "Actions",
            reason: "The action layer is not part of this build; nothing may mutate this file.",
        });
        out
    }
}

/// A §99.1 section this build cannot answer, with the reason it cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct UnansweredSection {
    pub section: &'static str,
    /// Cause, not apology. Rendered next to the section heading.
    pub reason: &'static str,
}

// ----------------------------------------------------------------- identity

/// §99.1 Identity.
#[derive(Clone, Debug, Serialize)]
pub struct Identity {
    /// Stable logical identity. Survives rename and move (FS-005).
    pub file_id: FileId,
    /// BLAKE3 of the current version's bytes. `None` when no version has been
    /// observed — a file row can exist before its first read.
    pub content_hash: Option<ContentHash>,
    pub size_bytes: Option<i64>,
    /// Probed, not guessed from the extension (FS-014). `None` when the prober
    /// did not identify it — never `"application/octet-stream"` as a stand-in.
    pub mime: Option<String>,
    /// Detected content language (I18N-001). `None` when undetected.
    pub language: Option<String>,
    /// `NULL` in the database while the file is deleted, so `None` here.
    pub current_path: Option<String>,
    pub status: FileStatus,
    pub origin: Origin,
    /// **Invariant #13.** `false` for agent-written files: findable, never
    /// citable.
    pub can_support_a_claim: bool,
    /// Other files whose current version has the same content hash (FS-008).
    /// Never contains this file. Empty means "we checked; there are none" —
    /// unlike the `None` sections above, this absence is a real answer.
    pub duplicates: Vec<Duplicate>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Another file with identical content.
///
/// Invariant #3: a shared hash is dedup information, not shared identity. These
/// are separate files that happen to hold the same bytes.
#[derive(Clone, Debug, Serialize)]
pub struct Duplicate {
    pub file_id: FileId,
    pub path: Option<String>,
    pub workspace: String,
}

// ----------------------------------------------------------------- location

/// §99.1 Location.
#[derive(Clone, Debug, Serialize)]
pub struct FileLocation {
    pub workspace_id: WorkspaceId,
    pub workspace: String,
    pub root_id: RootId,
    pub root_path: String,
    /// Current path with the root stripped. `None` when the file is deleted.
    pub relative_path: Option<String>,
    /// TIER-001 hydration state.
    pub tier_state: TierState,
    /// **Invariant #5.** `false` means the bytes must not be read: doing so
    /// would trigger a cloud download.
    pub safe_to_read: bool,
    /// `workspace_roots.storage_kind` — `LOCAL`, `TIERED_CLOUD`, …
    pub storage_kind: String,
    pub cloud_provider: Option<String>,
    /// FS-006, oldest first. More than one entry means the file has moved, and
    /// every version and chunk keyed on its `file_id` survived the move.
    pub path_history: Vec<PathEvent>,
}

/// One range of time during which the file lived at one path.
#[derive(Clone, Debug, Serialize)]
pub struct PathEvent {
    pub path: String,
    pub observed_from: Timestamp,
    /// `None` while this is the current path.
    pub observed_to: Option<Timestamp>,
}

// ----------------------------------------------------------------- versions

/// §99.1 Versions.
#[derive(Clone, Debug, Serialize)]
pub struct Versions {
    pub count: usize,
    /// The `CURRENT` version. `None` when none has been observed.
    pub current: Option<VersionSummary>,
    /// Every version, newest observation first, including the current one.
    pub history: Vec<VersionSummary>,
}

impl Versions {
    /// The `supersedes` chain from the current version backwards.
    ///
    /// Follows the recorded links rather than sorting by time: `observed_at`
    /// can tie inside one millisecond, and the chain is the fact the schema
    /// actually stores.
    pub fn supersedes_chain(&self) -> Vec<VersionId> {
        let mut chain = Vec::new();
        let mut next = self.current.as_ref().map(|v| v.version_id);
        while let Some(id) = next {
            if chain.contains(&id) {
                // A cycle means the write path corrupted the chain. Stop rather
                // than loop; the panel shows what it read, not a hang.
                tracing::warn!(version = %id, "cycle in the supersedes chain");
                break;
            }
            chain.push(id);
            next = self
                .history
                .iter()
                .find(|v| v.version_id == id)
                .and_then(|v| v.supersedes);
        }
        chain
    }
}

/// One observed state of a file's bytes.
#[derive(Clone, Debug, Serialize)]
pub struct VersionSummary {
    pub version_id: VersionId,
    pub content_hash: ContentHash,
    pub size_bytes: i64,
    pub mtime: Timestamp,
    pub observed_at: Timestamp,
    /// Where the file was when this version was observed. History, not identity.
    pub path_at_observation: String,
    pub supersedes: Option<VersionId>,
    pub status: VersionStatus,
    pub mime: Option<String>,
    pub language: Option<String>,
}

impl From<VersionRow> for VersionSummary {
    fn from(v: VersionRow) -> Self {
        Self {
            version_id: v.version_id,
            content_hash: v.content_hash,
            size_bytes: v.size_bytes,
            mtime: v.mtime_ms,
            observed_at: v.observed_at,
            path_at_observation: v.path_at_observation,
            supersedes: v.supersedes,
            status: v.status,
            mime: v.mime,
            language: v.language,
        }
    }
}

// -------------------------------------------------------------- index state

/// §99.1 "Text extraction health" and "Index state".
#[derive(Clone, Debug, Serialize)]
pub struct IndexState {
    /// Whether the current version produced anything searchable.
    pub parsed: bool,
    /// Active chunks on the current version.
    pub chunk_count: usize,
    /// The worst provenance class present across those chunks — what a citation
    /// badge shows (CONV-003). `None` when nothing is parsed.
    pub provenance_class: Option<ProvenanceClass>,
    /// Every provenance class present, best first.
    pub provenance_classes: Vec<ProvenanceClass>,
    /// `parse_results` for the current version (PAR-003, invariant #4:
    /// `(source_version, processor_id, processor_version)`).
    ///
    /// **`None` in practice on M1 data.** The table is in the schema and this
    /// reads it, but the M1 ingest path writes chunks without recording a parse
    /// result, so the parser's identity and version are not stored yet. That is
    /// why [`Self::chunker_versions`] exists: it is the one processor version
    /// M1 does persist.
    pub parse: Option<ParseState>,
    /// Distinct `chunks.chunker_version` values on the current version.
    pub chunker_versions: Vec<String>,
    /// Jobs still queued, leased or running against this file.
    ///
    /// Empty on M1 data: ingest is synchronous and enqueues no per-file job.
    /// The query is correct for when it does.
    pub pending_jobs: Vec<PendingJob>,
    /// Jobs that gave up, with the code and detail they died with (§111.1).
    pub errors: Vec<IndexError>,
}

/// One recorded parse attempt.
#[derive(Clone, Debug, Serialize)]
pub struct ParseState {
    pub parse_id: ParseId,
    pub parser_id: String,
    pub parser_version: String,
    /// `T1`..`T5` (Part 3 §63).
    pub parser_tier: String,
    pub provenance_class: ProvenanceClass,
    /// `OK`, `PARTIAL`, `LOW_YIELD`, `FAILED`, `UNSUPPORTED`, `SKIPPED_POLICY`,
    /// `METADATA_ONLY`.
    pub outcome: String,
    pub char_yield: Option<i64>,
    pub page_count: Option<i64>,
    /// Raw JSON as stored. Not parsed here — the panel shows what was recorded.
    pub warnings: Option<String>,
    pub parsed_at: Timestamp,
}

/// A job in flight against this file.
#[derive(Clone, Debug, Serialize)]
pub struct PendingJob {
    pub job_id: JobId,
    pub job_type: String,
    pub status: String,
    pub attempt: i64,
    pub max_attempts: i64,
}

/// A job that gave up.
#[derive(Clone, Debug, Serialize)]
pub struct IndexError {
    pub job_id: JobId,
    pub job_type: String,
    /// A §108 code as text, e.g. `PAR_TIMEOUT`. `None` when none was recorded.
    pub code: Option<String>,
    pub detail: Option<String>,
}

// ------------------------------------------------------------------- chunks

/// §99.1 Chunks: count and shape.
///
/// **Bodies are never included.** The panel says what is indexed; reading the
/// content is `marrow file … --chunks`, a different and more expensive request.
#[derive(Clone, Debug, Serialize)]
pub struct ChunkSummary {
    pub count: usize,
    /// Counts by `chunks.chunk_kind`, most frequent first.
    pub by_kind: Vec<KindCount>,
    pub total_tokens: i64,
    /// A few structural context prefixes (CHK-002) — enough to recognise the
    /// document's shape without reading it.
    pub sample_context: Vec<String>,
}

/// How many chunks of one kind.
#[derive(Clone, Debug, Serialize)]
pub struct KindCount {
    pub kind: String,
    pub count: usize,
}

// ------------------------------------------- types for sections M1 cannot fill
//
// These exist so the `Option` fields above have a shape to be `None` of. They
// are deliberately minimal: the real shape belongs to the milestone that fills
// them in, and guessing it now would be a schema decision made by the wrong
// crate at the wrong time.

/// One embedded metadata field the file states about itself (§69).
#[derive(Clone, Debug, Serialize)]
pub struct MetadataField {
    pub key: String,
    pub value: String,
    /// FI-002: every row states how it was extracted.
    pub extraction_method: String,
}

/// One entry in a document's structural outline (§8.6).
#[derive(Clone, Debug, Serialize)]
pub struct OutlineEntry {
    pub kind: String,
    pub title: String,
    /// Invariant #1: an outline entry you cannot navigate to is decoration.
    pub span: SourceSpan,
}

/// One entity this file mentions.
#[derive(Clone, Debug, Serialize)]
pub struct EntityMention {
    pub name: String,
    pub kind: String,
    /// Invariant #14: a fact never loses its authority class.
    pub authority: String,
}

/// One event on a file's timeline.
#[derive(Clone, Debug, Serialize)]
pub struct TimelineEvent {
    pub at: Timestamp,
    pub kind: String,
    pub detail: String,
}

// ----------------------------------------------------------------- assembly

/// Assemble the `FI` panel for one file.
///
/// **One read transaction** (LLD §2.7). Every section below is read from the
/// same snapshot, so the version count cannot disagree with the chunk count
/// because a write landed between two queries. The transaction is read-only and
/// rolls back on drop; on WAL that costs a snapshot, not a lock.
pub fn file_intelligence(store: &Store, file: FileRef) -> Result<FileIntelligence> {
    let reader = store.reader()?;
    let txn = reader
        .unchecked_transaction()
        .map_err(|e| map_sqlite(e, "Could not open a read snapshot of the index database."))?;
    let conn: &Connection = &txn;

    let file_id = match &file {
        FileRef::Id(id) => *id,
        FileRef::Path(path) => resolve_path(conn, path)?,
    };
    let row = read::find_file_by_id(conn, file_id)?.ok_or_else(|| not_in_index(&file))?;

    let versions = versions_of(conn, file_id)?;
    let workspace = workspace_of(conn, row.workspace_id)?;
    let root = root_of(conn, row.root_id)?;

    // Bound before the struct literal so the borrow of `versions.current` ends
    // before `versions` is moved into it.
    let current = versions.current.as_ref();
    let identity = identity_of(conn, &row, current)?;
    let location = location_of(conn, &row, &workspace, &root)?;
    let index_state = index_state_of(conn, &row, current)?;
    let chunks = chunks_of(conn, current)?;

    let panel = FileIntelligence {
        identity,
        location,
        index_state,
        chunks,
        versions,
        // Every one of these is `None` in this build. See the field docs: the
        // reason is carried, not the emptiness (FI-003).
        embedded_metadata: None,
        structure: None,
        entities: None,
        timeline: None,
    };
    tracing::debug!(
        file_id = %file_id,
        versions = panel.versions.count,
        chunks = panel.chunks.count,
        "file intelligence assembled"
    );
    Ok(panel)
}

fn not_in_index(file: &FileRef) -> Error {
    // §108 has no "unknown entity" class. FS_NOT_FOUND is the closest: the
    // thing the user named is not there, and the action is the same.
    let what = match file {
        FileRef::Id(id) => format!("file id {id}"),
        FileRef::Path(p) => format!("path {p:?}"),
    };
    Error::new(
        Code::FsNotFound,
        "That file is not in the index. Check the path, or run `marrow status` to see which \
         roots have been scanned.",
    )
    .with_context(what)
}

/// Resolve a path to a file id.
///
/// Ambiguity is an error, not a coin flip: the same path can legitimately be
/// indexed under two roots, and silently picking one would give a panel about
/// a file the user was not asking about.
fn resolve_path(conn: &Connection, path: &str) -> Result<FileId> {
    let mut stmt = conn
        .prepare("SELECT file_id, status FROM files WHERE current_path = ?1 ORDER BY file_id")
        .map_err(|e| map_sqlite(e, "Could not look a file up by path."))?;
    let rows = stmt
        .query_map(params![path], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| map_sqlite(e, "Could not look a file up by path."))?;

    let mut all: Vec<String> = Vec::new();
    let mut active: Vec<String> = Vec::new();
    for r in rows {
        let (id, status) = r.map_err(|e| map_sqlite(e, "Could not decode a file row."))?;
        if status == "ACTIVE" {
            active.push(id.clone());
        }
        all.push(id);
    }

    let candidates = if active.is_empty() { &all } else { &active };
    match candidates.len() {
        0 => Err(not_in_index(&FileRef::Path(path.to_string()))),
        1 => parse_id::<FileId>(&candidates[0], "files.file_id"),
        n => Err(Error::new(
            Code::CfgInvalid,
            "More than one indexed file is at that path, so it does not name one file. Use the \
             file id instead — `marrow search` prints it.",
        )
        .with_context(format!("{n} files at {path:?}"))),
    }
}

fn identity_of(
    conn: &Connection,
    row: &FileRow,
    current: Option<&VersionSummary>,
) -> Result<Identity> {
    let duplicates = match current {
        Some(v) => duplicates_of(conn, row.file_id, v.content_hash)?,
        None => Vec::new(),
    };
    Ok(Identity {
        file_id: row.file_id,
        content_hash: current.map(|v| v.content_hash),
        size_bytes: current.map(|v| v.size_bytes),
        mime: current.and_then(|v| v.mime.clone()),
        language: current.and_then(|v| v.language.clone()),
        current_path: row.current_path.clone(),
        status: row.status,
        origin: row.origin,
        can_support_a_claim: row.origin.can_support_a_claim(),
        duplicates,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// Files whose **current** version holds the same bytes (FS-008).
///
/// Current versions only: a file that once had these bytes and has since been
/// edited is not a duplicate of anything today.
fn duplicates_of(conn: &Connection, self_id: FileId, hash: ContentHash) -> Result<Vec<Duplicate>> {
    const SQL: &str = "SELECT DISTINCT f.file_id, f.current_path, w.name
                         FROM file_versions v
                         JOIN files f      ON f.file_id = v.file_id
                         JOIN workspaces w ON w.workspace_id = f.workspace_id
                        WHERE v.content_hash = ?1
                          AND v.status = 'CURRENT'
                          AND f.file_id <> ?2
                          AND f.status <> 'FORGOTTEN'
                        ORDER BY f.file_id";
    let mut stmt = conn
        .prepare(SQL)
        .map_err(|e| map_sqlite(e, "Could not look for files with identical content."))?;
    let rows = stmt
        .query_map(params![hash.to_hex(), self_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| map_sqlite(e, "Could not look for files with identical content."))?;

    let mut out = Vec::new();
    for r in rows {
        let (id, path, workspace) =
            r.map_err(|e| map_sqlite(e, "Could not decode a duplicate-file row."))?;
        out.push(Duplicate {
            file_id: parse_id(&id, "files.file_id")?,
            path,
            workspace,
        });
    }
    Ok(out)
}

fn location_of(
    conn: &Connection,
    row: &FileRow,
    workspace: &crate::search::WorkspaceInfo,
    root: &RootInfo,
) -> Result<FileLocation> {
    let history = read::path_history(conn, row.file_id)?
        .into_iter()
        .map(|p| PathEvent {
            path: p.path,
            observed_from: p.observed_from,
            observed_to: p.observed_to,
        })
        .collect();
    Ok(FileLocation {
        workspace_id: row.workspace_id,
        workspace: workspace.name.clone(),
        root_id: row.root_id,
        relative_path: row
            .current_path
            .as_deref()
            .map(|p| relative_path(p, std::slice::from_ref(&root.canonical_path))),
        root_path: root.canonical_path.clone(),
        tier_state: row.tier_state,
        safe_to_read: row.tier_state.safe_to_read(),
        storage_kind: root.storage_kind.clone(),
        cloud_provider: root.cloud_provider.clone(),
        path_history: history,
    })
}

/// The bits of `workspace_roots` the panel shows.
#[derive(Clone, Debug)]
pub(crate) struct RootInfo {
    pub canonical_path: String,
    pub storage_kind: String,
    pub cloud_provider: Option<String>,
}

fn root_of(conn: &Connection, root_id: RootId) -> Result<RootInfo> {
    conn.query_row(
        "SELECT canonical_path, storage_kind, cloud_provider FROM workspace_roots
          WHERE root_id = ?1",
        params![root_id.to_string()],
        |r| {
            Ok(RootInfo {
                canonical_path: r.get(0)?,
                storage_kind: r.get(1)?,
                cloud_provider: r.get(2)?,
            })
        },
    )
    .map_err(|e| map_sqlite(e, "Could not read the root a file was found under."))
}

fn versions_of(conn: &Connection, file_id: FileId) -> Result<Versions> {
    let history: Vec<VersionSummary> = read::versions_for(conn, file_id)?
        .into_iter()
        .map(VersionSummary::from)
        .collect();
    let current = history
        .iter()
        .find(|v| v.status == VersionStatus::Current)
        .cloned();
    Ok(Versions {
        count: history.len(),
        current,
        history,
    })
}

fn index_state_of(
    conn: &Connection,
    row: &FileRow,
    current: Option<&VersionSummary>,
) -> Result<IndexState> {
    let (chunk_count, provenance_classes, chunker_versions) = match current {
        Some(v) => chunk_facts(conn, v.version_id)?,
        None => (0, Vec::new(), Vec::new()),
    };
    let parse = match current {
        Some(v) => parse_state(conn, v.version_id)?,
        None => None,
    };
    let (pending_jobs, errors) = jobs_for(conn, row.file_id)?;
    Ok(IndexState {
        // Either a recorded parse attempt or something searchable counts as
        // "parsed". A parse that yielded nothing is still a parse, and saying
        // otherwise would hide a `LOW_YIELD` outcome behind a plain "no".
        parsed: parse.is_some() || chunk_count > 0,
        chunk_count,
        // `ProvenanceClass` orders Exact < Degraded < Approximate <
        // MetadataOnly, so `max` is the worst — which is what a badge must show.
        provenance_class: provenance_classes.iter().copied().max(),
        provenance_classes,
        parse,
        chunker_versions,
        pending_jobs,
        errors,
    })
}

type ChunkFacts = (usize, Vec<ProvenanceClass>, Vec<String>);

fn chunk_facts(conn: &Connection, version_id: VersionId) -> Result<ChunkFacts> {
    let mut stmt = conn
        .prepare(
            "SELECT count(*), group_concat(DISTINCT provenance_class),
                    group_concat(DISTINCT chunker_version)
               FROM chunks WHERE version_id = ?1 AND status = 'ACTIVE'",
        )
        .map_err(|e| map_sqlite(e, "Could not read a file's index state."))?;
    let (count, provenance, chunkers): (i64, Option<String>, Option<String>) = stmt
        .query_row(params![version_id.to_string()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(|e| map_sqlite(e, "Could not read a file's index state."))?;

    let mut classes: Vec<ProvenanceClass> = provenance
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .filter_map(provenance_of)
        .collect();
    classes.sort_unstable();
    classes.dedup();

    let mut chunker_versions: Vec<String> = chunkers
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    chunker_versions.sort();
    chunker_versions.dedup();

    Ok((count.max(0) as usize, classes, chunker_versions))
}

fn parse_state(conn: &Connection, version_id: VersionId) -> Result<Option<ParseState>> {
    conn.query_row(
        "SELECT parse_id, parser_id, parser_version, parser_tier, provenance_class, outcome,
                char_yield, page_count, warnings, parsed_at
           FROM parse_results WHERE version_id = ?1
          ORDER BY parsed_at DESC, parse_id DESC LIMIT 1",
        params![version_id.to_string()],
        |r| {
            let raw_class: String = r.get(4)?;
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                raw_class,
                r.get::<_, String>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, i64>(9)?,
            ))
        },
    )
    .optional()
    .map_err(|e| map_sqlite(e, "Could not read a file's parse result."))?
    .map(|t| {
        Ok(ParseState {
            parse_id: parse_id::<ParseId>(&t.0, "parse_results.parse_id")?,
            parser_id: t.1,
            parser_version: t.2,
            parser_tier: t.3,
            // A class the schema's CHECK allows but this build does not know is
            // impossible; falling back to MetadataOnly rather than guessing
            // `Exact` keeps an unknown from being rendered as a stronger claim.
            provenance_class: provenance_of(&t.4).unwrap_or(ProvenanceClass::MetadataOnly),
            outcome: t.5,
            char_yield: t.6,
            page_count: t.7,
            warnings: t.8,
            parsed_at: Timestamp::from_millis(t.9),
        })
    })
    .transpose()
}

type JobLists = (Vec<PendingJob>, Vec<IndexError>);

fn jobs_for(conn: &Connection, file_id: FileId) -> Result<JobLists> {
    let mut stmt = conn
        .prepare(
            "SELECT job_id, job_type, status, attempt, max_attempts,
                    last_error_code, last_error_detail
               FROM jobs WHERE target_id = ?1 ORDER BY created_at, job_id",
        )
        .map_err(|e| map_sqlite(e, "Could not read the jobs queued for a file."))?;
    let rows = stmt
        .query_map(params![file_id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })
        .map_err(|e| map_sqlite(e, "Could not read the jobs queued for a file."))?;

    let mut pending = Vec::new();
    let mut errors = Vec::new();
    for r in rows {
        let (id, job_type, status, attempt, max_attempts, code, detail) =
            r.map_err(|e| map_sqlite(e, "Could not decode a job row."))?;
        let job_id = parse_id::<JobId>(&id, "jobs.job_id")?;
        match status.as_str() {
            "PENDING" | "LEASED" | "RUNNING" => pending.push(PendingJob {
                job_id,
                job_type,
                status,
                attempt,
                max_attempts,
            }),
            "DEAD" | "FAILED" => errors.push(IndexError {
                job_id,
                job_type,
                code,
                detail,
            }),
            // DONE and CANCELLED are neither pending nor a problem.
            _ => {}
        }
    }
    Ok((pending, errors))
}

fn chunks_of(conn: &Connection, current: Option<&VersionSummary>) -> Result<ChunkSummary> {
    let Some(v) = current else {
        return Ok(ChunkSummary {
            count: 0,
            by_kind: Vec::new(),
            total_tokens: 0,
            sample_context: Vec::new(),
        });
    };
    let version_id = v.version_id.to_string();

    let mut stmt = conn
        .prepare(
            "SELECT chunk_kind, count(*) AS n, COALESCE(sum(token_count), 0)
               FROM chunks WHERE version_id = ?1 AND status = 'ACTIVE'
              GROUP BY chunk_kind ORDER BY n DESC, chunk_kind ASC",
        )
        .map_err(|e| map_sqlite(e, "Could not summarise a file's chunks."))?;
    let rows = stmt
        .query_map(params![version_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| map_sqlite(e, "Could not summarise a file's chunks."))?;

    let mut by_kind = Vec::new();
    let mut count = 0usize;
    let mut total_tokens = 0i64;
    for r in rows {
        let (kind, n, tokens) =
            r.map_err(|e| map_sqlite(e, "Could not decode a chunk summary."))?;
        count += n.max(0) as usize;
        total_tokens += tokens;
        by_kind.push(KindCount {
            kind,
            count: n.max(0) as usize,
        });
    }

    // Context prefixes only — the structural breadcrumb (CHK-002), never a
    // chunk body. The panel describes what is indexed; it is not a reader.
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT context_prefix FROM chunks
              WHERE version_id = ?1 AND status = 'ACTIVE'
                AND context_prefix IS NOT NULL AND context_prefix <> ''
              ORDER BY context_prefix LIMIT 5",
        )
        .map_err(|e| map_sqlite(e, "Could not read a file's chunk outline."))?;
    let rows = stmt
        .query_map(params![v.version_id.to_string()], |r| r.get::<_, String>(0))
        .map_err(|e| map_sqlite(e, "Could not read a file's chunk outline."))?;
    let mut sample_context = Vec::new();
    for r in rows {
        sample_context.push(r.map_err(|e| map_sqlite(e, "Could not decode a chunk prefix."))?);
    }

    Ok(ChunkSummary {
        count,
        by_kind,
        total_tokens,
        sample_context,
    })
}

// -------------------------------------------------------------------- codecs

/// `chunks.provenance_class` / `parse_results.provenance_class` (§106.1: enums
/// are SCREAMING TEXT with a CHECK).
fn provenance_of(s: &str) -> Option<ProvenanceClass> {
    Some(match s.trim() {
        "EXACT" => ProvenanceClass::Exact,
        "DEGRADED" => ProvenanceClass::Degraded,
        "APPROXIMATE" => ProvenanceClass::Approximate,
        "METADATA_ONLY" => ProvenanceClass::MetadataOnly,
        _ => return None,
    })
}

/// Decode a typed ULID out of a `TEXT` column.
///
/// Bounded on `FromStr` rather than on `ulid::DecodeError` so this crate does
/// not take a `ulid` dependency to name an error it only ever formats.
fn parse_id<T: std::str::FromStr>(s: &str, column: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    s.parse::<T>().map_err(|e| {
        Error::new(
            Code::DbCorrupt,
            "The index database holds an identifier that is not a ULID. Delete the index \
             directory to rebuild it from your files.",
        )
        .with_context(format!("{column} = {s:?}: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(
        id: VersionId,
        supersedes: Option<VersionId>,
        status: VersionStatus,
    ) -> VersionSummary {
        VersionSummary {
            version_id: id,
            content_hash: ContentHash::of(b"x"),
            size_bytes: 1,
            mtime: Timestamp::EPOCH,
            observed_at: Timestamp::EPOCH,
            path_at_observation: "/a".into(),
            supersedes,
            status,
            mime: None,
            language: None,
        }
    }

    #[test]
    fn the_supersedes_chain_walks_recorded_links_not_timestamps() {
        let v1 = VersionId::new();
        let v2 = VersionId::new();
        let v3 = VersionId::new();
        let versions = Versions {
            count: 3,
            current: Some(version(v3, Some(v2), VersionStatus::Current)),
            history: vec![
                version(v3, Some(v2), VersionStatus::Current),
                version(v2, Some(v1), VersionStatus::Historical),
                version(v1, None, VersionStatus::Historical),
            ],
        };
        assert_eq!(versions.supersedes_chain(), vec![v3, v2, v1]);
    }

    #[test]
    fn a_cycle_in_the_chain_terminates_instead_of_hanging() {
        let a = VersionId::new();
        let b = VersionId::new();
        let versions = Versions {
            count: 2,
            current: Some(version(a, Some(b), VersionStatus::Current)),
            history: vec![
                version(a, Some(b), VersionStatus::Current),
                version(b, Some(a), VersionStatus::Historical),
            ],
        };
        assert_eq!(versions.supersedes_chain().len(), 2);
    }

    #[test]
    fn provenance_codec_matches_the_schema_check() {
        for (sql, want) in [
            ("EXACT", ProvenanceClass::Exact),
            ("DEGRADED", ProvenanceClass::Degraded),
            ("APPROXIMATE", ProvenanceClass::Approximate),
            ("METADATA_ONLY", ProvenanceClass::MetadataOnly),
        ] {
            assert_eq!(provenance_of(sql), Some(want));
        }
        assert_eq!(provenance_of("exact"), None, "the schema stores SCREAMING");
        assert_eq!(provenance_of("nonsense"), None);
    }

    #[test]
    fn the_worst_provenance_class_is_the_one_a_badge_shows() {
        let mut classes = [ProvenanceClass::Exact, ProvenanceClass::Approximate];
        classes.sort_unstable();
        assert_eq!(
            classes.iter().copied().max(),
            Some(ProvenanceClass::Approximate)
        );
    }

    #[test]
    fn a_bad_ulid_is_a_corruption_error_not_a_panic() {
        let err = parse_id::<marrow_core::ChunkId>("not-a-ulid", "chunks.chunk_id").unwrap_err();
        assert_eq!(err.code(), Code::DbCorrupt);
        assert!(err.context().is_some(), "the bad value is kept for logs");
    }
}
