//! The `ContentParser` port and its inputs (LLD §2.2, §6).

use marrow_core::{Result, TierState};

use crate::budget::BudgetGuard;
use crate::ir::{ParsedArtifact, ParserTier};

/// The cheap facts a parser is allowed to route on.
///
/// This mirrors `marrow_scan::probe::FileFacts` **without depending on it**.
/// `marrow-parse` and `marrow-scan` are siblings in the dependency graph
/// (LLD §1); a lateral edge would drag `std::fs`, `ignore` and path
/// canonicalisation into a crate whose entire job is "bytes in, IR out".
/// The orchestration layer maps one to the other — that mapping is four field
/// copies and it keeps every parser testable from a string literal.
///
/// Note what is *not* here: no path. Path is never identity — a path is
/// history, not
/// identity — and a parser has no business knowing where a file lives. The file
/// name survives because extensions route, and because a name like
/// `Cargo.lock` or `.gitignore` is a real routing signal that an extension is
/// not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileProbe {
    /// Base name only, no directory components.
    pub file_name: String,
    /// Lowercased extension, without the dot. `None` for the 97 extensionless
    /// files M0 found.
    pub extension: Option<String>,
    /// Size in bytes as reported by `lstat`.
    pub size: u64,
    /// Extension-derived MIME guess. **Never authoritative** (FS-014); a `.txt`
    /// holding a ZIP still hints `text/plain`, which is exactly why every
    /// parser confirms against the bytes before committing.
    pub mime_hint: Option<String>,
    /// **Never hydrate a placeholder.** A non-`Resident` file is never content-parsed.
    pub tier: TierState,
}

impl FileProbe {
    /// Build a probe from a file name and size. The common case in tests and
    /// the only thing a parser actually needs.
    pub fn new(file_name: impl Into<String>, size: u64) -> Self {
        let file_name: String = file_name.into();
        let extension = extension_of(&file_name);
        Self {
            file_name,
            extension,
            size,
            mime_hint: None,
            tier: TierState::Resident,
        }
    }

    pub fn with_mime_hint(mut self, hint: impl Into<String>) -> Self {
        self.mime_hint = Some(hint.into());
        self
    }

    pub fn with_tier(mut self, tier: TierState) -> Self {
        self.tier = tier;
        self
    }

    /// Case-insensitive extension test.
    pub fn has_extension(&self, ext: &str) -> bool {
        self.extension.as_deref() == Some(ext)
    }

    pub fn has_any_extension(&self, exts: &[&str]) -> bool {
        exts.iter().any(|e| self.has_extension(e))
    }
}

/// Split a base name into its lowercased extension.
///
/// Dotfiles have no extension: `.gitignore` is a name, not an extension of an
/// empty-named file. `std::path::Path::extension` agrees; this does not use it
/// only because a `FileProbe` deliberately holds no path.
fn extension_of(file_name: &str) -> Option<String> {
    let name = file_name.rsplit('/').next().unwrap_or(file_name);
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// Everything a parser gets. Bytes plus facts — never a path, never a handle.
///
/// Reading files is `marrow-scan`'s job, and it has already answered the one
/// question that matters (never hydrate a placeholder) by the time these
/// bytes exist. Keeping
/// the parser at arm's length from the filesystem is also what makes every test
/// in this crate a string literal rather than a tempdir.
#[derive(Clone, Copy, Debug)]
pub struct ParseInput<'a> {
    pub bytes: &'a [u8],
    pub probe: &'a FileProbe,
    /// Fresh per parser attempt, so a slow first parser does not spend the
    /// second one's time budget.
    pub budget: BudgetGuard,
}

/// A content parser (LLD §6, one of the four seams).
///
/// Implementations know nothing about each other. Ordering, fallback and
/// failure policy belong to [`crate::router::ParserRouter`].
///
/// **Not `async`.** LLD §4: async lives at the adapter edge and nowhere below
/// it. Parsing is CPU-bound work on bytes already in memory; there is nothing
/// here to multiplex.
pub trait ContentParser: Send + Sync + std::fmt::Debug {
    /// Stable identity, persisted with every result (PAR-003).
    fn id(&self) -> &'static str;

    /// Bumped whenever output would change for the same input. Drives automatic
    /// reprocessing after an upgrade — the processor-version rule.
    fn version(&self) -> &'static str;

    fn tier(&self) -> ParserTier;

    /// Cheap routing check. **Must not read the bytes** — it only gets a probe.
    ///
    /// A `true` here is a claim about the file's name, not its content. Every
    /// parser is expected to confirm against the bytes inside `parse` and
    /// return `Code::ParUnsupported` when the guess was wrong; that is the
    /// mechanism that makes FS-014 ("the extension is not the classifier")
    /// safe rather than merely acknowledged.
    fn handles(&self, probe: &FileProbe) -> bool;

    /// Parse, or explain why not.
    ///
    /// | Returning | Router does |
    /// |---|---|
    /// | `Ok(artifact)` | takes it, stops |
    /// | `Err(ParUnsupported)` | tries the next parser, silently |
    /// | `Err(_)` where `isolates_to_one_file()` | warns, records, tries the next |
    /// | any other `Err` | propagates; the run stops |
    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedArtifact>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_lowercased_and_dotfiles_have_none() {
        assert_eq!(extension_of("main.RS").as_deref(), Some("rs"));
        assert_eq!(extension_of("archive.tar.gz").as_deref(), Some("gz"));
        assert_eq!(extension_of(".gitignore"), None);
        assert_eq!(extension_of("README"), None);
        assert_eq!(extension_of("trailing."), None);
    }

    #[test]
    fn a_probe_carries_no_path() {
        // Path is never identity, enforced by the absence of a field. If a future edit
        // adds `path: PathBuf` here, this test is the place it should be
        // argued out, not the place it quietly compiles.
        let p = FileProbe::new("notes.md", 12);
        assert_eq!(p.file_name, "notes.md");
        assert!(p.has_extension("md"));
        assert_eq!(p.tier, TierState::Resident);
    }
}
