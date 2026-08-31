//! Core domain types.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Milliseconds since the Unix epoch, **UTC always**.
///
/// Never local time, never a string. Local time in a database is a bug that
/// surfaces six months later at a DST boundary.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    pub const EPOCH: Timestamp = Timestamp(0);

    pub fn now() -> Self {
        Self(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        )
    }

    pub const fn from_millis(ms: i64) -> Self {
        Self(ms)
    }

    pub const fn as_millis(self) -> i64 {
        self.0
    }

    /// From a filesystem mtime. Pre-epoch times clamp to 0 rather than going
    /// negative — some archives and sync clients produce nonsense mtimes.
    pub fn from_system_time(t: std::time::SystemTime) -> Self {
        Self(
            t.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        )
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({}ms)", self.0)
    }
}

/// A BLAKE3 content digest.
///
/// Identity for *content* (dedup, embedding cache), never for *files*. Two
/// files may legitimately share a digest — see [`crate::id::FileId`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub const fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            use fmt::Write;
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16)?;
            let lo = (chunk[1] as char).to_digit(16)?;
            out[i] = (hi * 16 + lo) as u8;
        }
        Some(Self(out))
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Short form: enough to correlate in a log, not enough to be noise.
        write!(f, "blake3:{}…", &self.to_hex()[..12])
    }
}

impl Serialize for ContentHash {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'_, str> as Deserialize>::deserialize(d)?;
        Self::from_hex(&s).ok_or_else(|| serde::de::Error::custom("bad blake3 hex"))
    }
}

/// **A `source_span` on every IR node.** Where in a source artifact something came from.
///
/// Every IR node carries one. Provenance to an exact location is the entire
/// reason this project exists rather than `ripgrep | llm`, it is nearly free to
/// record at write time, and it is nearly impossible to add afterwards.
///
/// Variants are per-format because "page 17" and "cell B4" are not the same
/// kind of fact and flattening them to a byte offset destroys both.
// No `Eq`: the PDF bbox carries `f32`. Spans are compared for equality in
// tests and diffing, never used as a map key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceSpan {
    /// Byte range in the decoded content. Text, Markdown, code, CSV.
    Bytes { start: u64, end: u64 },
    /// Line range, 1-based inclusive. Companion to `Bytes` for display.
    Lines { start: u32, end: u32 },
    /// Page plus optional bounding box, in PDF points.
    ///
    /// No longer deferred: `marrow_parse::pdf` emits this from PDFKit
    /// character bounds ([D54]). The "Deferred (M0 F3)" note that stood here
    /// outlived the parser by months, and `docs/Comparison.md` §13.1 was still
    /// citing it as evidence the claim was unmet.
    Page { page: u32, bbox: Option<[f32; 4]> },
    /// Sheet and A1 range. Spreadsheets.
    Cells { sheet: String, range: String },
    /// Path within an XML tree. OOXML.
    XPath { path: String },
    /// Time range in milliseconds. Audio, video, transcripts.
    Time { start_ms: u64, end_ms: u64 },
    /// The file as a whole. Only legitimate for whole-file metadata — never a
    /// fallback for "I could not be bothered to track position".
    Whole,
}

impl SourceSpan {
    /// `path` plus wherever in it this span points, in the notation the format
    /// itself uses — `report.md:42`, `contract.pdf:p17`, `q2.xlsx:Sheet1!B4`.
    ///
    /// **One implementation, because there were two and they disagreed.** The
    /// MCP server rendered `Page` and `Cells` correctly; the desktop's copy in
    /// `state.rs` matched only `Lines` and fell through to the bare filename
    /// for everything else. So the same citation, for the same chunk, read as
    /// `contract.pdf:p17` through an agent and as `contract.pdf` in the app —
    /// with the page silently gone on the surface whose entire promise is
    /// citing one.
    ///
    /// `Bytes`, `XPath`, `Time` and `Whole` deliberately render as the path
    /// alone. A byte offset is not a location a person can act on, and the
    /// other three have no reader yet; inventing a notation for them here
    /// would be a claim about precision that nothing behind it supports.
    /// Read a span back from the JSON it is persisted as.
    ///
    /// `None` for anything that will not parse. Callers are asking "where does
    /// this point", and a malformed span answers that with "nowhere I can
    /// tell" rather than with an error nobody can act on — the row is already
    /// written and a query is not the place to discover it is corrupt.
    pub fn from_json(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }

    pub fn locate(&self, path: &str) -> String {
        match self {
            Self::Lines { start, .. } => format!("{path}:{start}"),
            Self::Page { page, .. } => format!("{path}:p{page}"),
            Self::Cells { sheet, range } => format!("{path}:{sheet}!{range}"),
            _ => path.to_string(),
        }
    }

    /// Whether this span points at a specific location a human can be taken to.
    ///
    /// `Whole` is honest but not navigable; a citation that resolves to
    /// "somewhere in this file" is not the product's promise.
    pub fn is_precise(&self) -> bool {
        !matches!(self, SourceSpan::Whole)
    }
}

/// Cloud-sync hydration state (TIER-001). **Never hydrate a placeholder.**
///
/// Reading a `Placeholder` triggers a download. On a large sync folder that is
/// hundreds of gigabytes of someone's bandwidth, silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TierState {
    /// Bytes are on local disk. Safe to read.
    Resident,
    /// Dehydrated stub. **Never read this without explicit user opt-in.**
    Placeholder,
    /// Currently materialising.
    Hydrating,
    /// Volume detached or otherwise unreachable.
    Unavailable,
}

impl TierState {
    /// The one question every read path must ask first.
    pub fn safe_to_read(self) -> bool {
        matches!(self, TierState::Resident)
    }
}

/// Where a file came from. **The `origin = SELF` rule.**
///
/// `SelfWritten` content is indexed and searchable — you must be able to find
/// what the agent wrote — but it can never support a claim. Otherwise a summary
/// written into a watched folder gets re-indexed and cites itself back as
/// independent corroboration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Authored outside this system.
    User,
    /// Produced by an agent action, recipe or generator.
    SelfWritten,
}

impl Origin {
    /// Whether content of this origin may be cited as evidence for a claim.
    pub fn can_support_a_claim(self) -> bool {
        matches!(self, Origin::User)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Active,
    Deleted,
    Excluded,
    Error,
    Forgotten,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    Current,
    Historical,
    Tombstoned,
}

/// Fidelity of a citation (CONV-003).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceClass {
    /// Native parse. Byte range, cell reference or AST node.
    Exact,
    /// Converted through a lossy path; file and heading only.
    Degraded,
    /// Reconstructed — OCR, coordinate clustering. Cite with a badge.
    Approximate,
    /// No content was parsed; metadata only.
    MetadataOnly,
}

impl ProvenanceClass {
    /// Retrieval down-weighting (Part 6 §113.3).
    pub fn rank_multiplier(self) -> f32 {
        match self {
            ProvenanceClass::Exact => 1.0,
            ProvenanceClass::Degraded => 0.8,
            ProvenanceClass::Approximate => 0.6,
            ProvenanceClass::MetadataOnly => 0.5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_and_a_cell_survive_into_the_citation_a_person_reads() {
        // Two implementations of this existed and they disagreed. The MCP
        // server rendered `Page` and `Cells`; the desktop's copy matched only
        // `Lines` and let everything else fall through to the bare filename —
        // so the same chunk cited `contract.pdf:p17` through an agent and
        // `contract.pdf` in the app, with the page gone on the surface whose
        // entire promise is citing one.
        assert_eq!(
            SourceSpan::Page {
                page: 17,
                bbox: Some([72.0, 90.0, 520.0, 140.0])
            }
            .locate("contract.pdf"),
            "contract.pdf:p17"
        );
        assert_eq!(
            SourceSpan::Cells {
                sheet: "Q2".into(),
                range: "B4:B18".into()
            }
            .locate("q2.xlsx"),
            "q2.xlsx:Q2!B4:B18"
        );
        assert_eq!(
            SourceSpan::Lines { start: 42, end: 44 }.locate("report.md"),
            "report.md:42"
        );

        // And the ones with no notation a person can act on stay the path.
        for span in [SourceSpan::Bytes { start: 0, end: 9 }, SourceSpan::Whole] {
            assert_eq!(span.locate("notes.txt"), "notes.txt", "{span:?}");
        }
    }

    #[test]
    fn placeholders_are_never_safe_to_read() {
        assert!(TierState::Resident.safe_to_read());
        for t in [
            TierState::Placeholder,
            TierState::Hydrating,
            TierState::Unavailable,
        ] {
            assert!(!t.safe_to_read(), "{t:?} must not be read");
        }
    }

    #[test]
    fn self_written_content_cannot_support_a_claim() {
        assert!(Origin::User.can_support_a_claim());
        assert!(!Origin::SelfWritten.can_support_a_claim());
    }

    #[test]
    fn whole_file_spans_are_not_precise() {
        assert!(!SourceSpan::Whole.is_precise());
        assert!(SourceSpan::Bytes { start: 0, end: 10 }.is_precise());
        assert!(SourceSpan::Cells {
            sheet: "Q2".into(),
            range: "B4:B18".into()
        }
        .is_precise());
    }

    #[test]
    fn hash_hex_round_trips() {
        let h = ContentHash::of(b"marrow");
        assert_eq!(h.to_hex().len(), 64);
        assert_eq!(Some(h), ContentHash::from_hex(&h.to_hex()));
        assert_eq!(ContentHash::from_hex("nonsense"), None);
    }

    #[test]
    fn hash_is_stable_and_distinguishing() {
        assert_eq!(ContentHash::of(b"a"), ContentHash::of(b"a"));
        assert_ne!(ContentHash::of(b"a"), ContentHash::of(b"b"));
    }

    #[test]
    fn source_spans_survive_json() {
        let spans = [
            SourceSpan::Bytes { start: 4, end: 90 },
            SourceSpan::Lines { start: 1, end: 3 },
            SourceSpan::Page {
                page: 17,
                bbox: Some([72.0, 410.0, 520.0, 566.0]),
            },
            SourceSpan::Cells {
                sheet: "Q2".into(),
                range: "B4:B18".into(),
            },
            SourceSpan::XPath {
                path: "/w:document/w:body/w:tbl[2]".into(),
            },
            SourceSpan::Time {
                start_ms: 872_000,
                end_ms: 881_500,
            },
            SourceSpan::Whole,
        ];
        for s in spans {
            let j = serde_json::to_string(&s).unwrap();
            assert_eq!(
                s,
                serde_json::from_str(&j).unwrap(),
                "round trip failed: {j}"
            );
        }
    }

    #[test]
    fn degraded_provenance_ranks_below_exact() {
        assert!(
            ProvenanceClass::Exact.rank_multiplier() > ProvenanceClass::Degraded.rank_multiplier()
        );
        assert!(
            ProvenanceClass::Degraded.rank_multiplier()
                > ProvenanceClass::Approximate.rank_multiplier()
        );
    }

    #[test]
    fn timestamps_are_utc_millis_and_never_negative() {
        let pre_epoch = std::time::UNIX_EPOCH - std::time::Duration::from_secs(86_400);
        assert_eq!(Timestamp::from_system_time(pre_epoch).as_millis(), 0);
        assert!(Timestamp::now().as_millis() > 1_700_000_000_000);
    }
}
