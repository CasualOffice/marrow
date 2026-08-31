//! What a caller is allowed to *name*, decided before the disk is touched.
//!
//! This module answers one question: is this string a path we are willing to
//! turn into a file inside a workspace? It never looks at the filesystem, so it
//! is not a containment check — [`crate::guard`] does that against real inodes.
//! It exists because several attacks are visible in the string alone, and
//! catching them here means they never reach a `mkdir`.
//!
//! Two refusal families, and the split is deliberate because a caller has to
//! act differently on each:
//!
//! - [`Code::FsPathEscapeBlocked`] — the name is *trying to leave*. `..`, an
//!   absolute path, a percent-escape that decodes to a separator, or a
//!   character that folds to a dot. There is no legitimate version of this
//!   request.
//! - [`Code::ActNameRejected`] — the name is inside the workspace but is one we will
//!   not create: a NUL byte, a control character, a reserved device name, or
//!   something longer than a filesystem will store. The caller should pick a
//!   different name.
//!
//! The Unicode rules are the non-obvious ones. `..%2f` is inert to `open(2)`,
//! and `‥` is a perfectly legal directory name on APFS — neither escapes
//! anything *here*. They are refused because this path string is about to be
//! handed to other software (a shell, a URL, a zip writer, a sync client), and
//! the first one of those that decodes or NFKC-folds it gets a `..` it did not
//! expect. Refusing costs nothing; the name has no honest use.

use marrow_core::{Code, Error, Result};
use unicode_normalization::UnicodeNormalization;

/// Longest single filename component, in bytes. APFS, HFS+, ext4 and NTFS all
/// stop at 255; going over produces `ENAMETOOLONG` from the syscall, which
/// would reach the user as the generic "file could not be read" mapping in
/// `marrow_core::Error::from(io::Error)`. Refusing here keeps the message
/// specific.
const MAX_COMPONENT_BYTES: usize = 255;

/// Longest whole relative path, in bytes. macOS `PATH_MAX` is 1024 including
/// the root prefix, so a relative path near that length is unusable regardless
/// of which workspace it lands in.
const MAX_PATH_BYTES: usize = 900;

/// Names Windows reserves as devices, in any case, with or without an
/// extension. macOS will happily create `COM1.md`; a sync client, a zip
/// extracted on Windows, or a `git checkout` on a colleague's machine will not.
const RESERVED_STEMS: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

fn escape(name: &str, why: &str) -> Error {
    Error::new(
        Code::FsPathEscapeBlocked,
        "Refused to write to a path that points outside the workspace. Name the file \
         relative to the workspace root, with no `.` or `..` segments and no leading `/`.",
    )
    .with_context(format!("{name} — {why}"))
}

fn denied(name: &str, why: impl AsRef<str>, action: &str) -> Error {
    let why = why.as_ref();
    Error::new(Code::ActNameRejected, format!("{why} {action}")).with_context(name.to_string())
}

/// Split a caller-supplied relative path into components, refusing anything we
/// will not create.
///
/// Returns the components in order; the last is the filename.
pub(crate) fn validate(relative: &str) -> Result<Vec<String>> {
    if relative.contains('\0') {
        // Checked first: a NUL truncates the name at the syscall boundary, so
        // every rule below it would be reasoning about a different path than
        // the one that would be created.
        return Err(denied(
            &relative.replace('\0', "\\0"),
            "The file name contains a NUL byte, which would silently truncate it at the \
             filesystem boundary.",
            "Remove it from the name.",
        ));
    }
    if relative.trim().is_empty() {
        return Err(denied(
            relative,
            "No file name was given.",
            "Name the file relative to the workspace root, e.g. `notes/summary.md`.",
        ));
    }
    if relative.starts_with('/') {
        return Err(escape(relative, "it is an absolute path"));
    }
    if relative.len() > MAX_PATH_BYTES {
        return Err(denied(
            &elide(relative),
            format!(
                "The path is {} bytes long, over the {MAX_PATH_BYTES}-byte limit.",
                relative.len()
            ),
            "Write it to a shallower directory or shorten the name.",
        ));
    }

    let mut out = Vec::new();
    for component in relative.split('/') {
        validate_component(relative, component)?;
        out.push(component.to_string());
    }
    Ok(out)
}

fn validate_component(whole: &str, c: &str) -> Result<()> {
    if c.is_empty() {
        return Err(denied(
            whole,
            "The path has an empty segment (`//` or a trailing `/`).",
            "Give one name per directory level.",
        ));
    }
    if c == "." || c == ".." {
        return Err(escape(whole, "it contains a `.` or `..` segment"));
    }

    // A percent-escape is not a filesystem feature. If one decodes to a
    // separator or to `..`, the name is aimed at whatever decodes it next.
    let decoded = percent_decode(c);
    if decoded != c && (decoded.contains('/') || decoded.contains('\\') || is_dots(&decoded)) {
        return Err(escape(
            whole,
            "a percent-escape in it decodes to a path separator or `..`",
        ));
    }

    // NFKC is the fold that turns `‥` (U+2025) into `..` and `．` (U+FF00
    // block) into `.`. Compatibility, not canonical: NFC leaves both alone,
    // which is exactly why NFC-only checking misses this.
    let folded: String = c.nfkc().collect();
    if is_dots(&folded) || folded.contains('/') {
        return Err(escape(
            whole,
            "a character in it folds to `.`, `..` or a path separator",
        ));
    }

    if let Some(ch) = c.chars().find(|ch| ch.is_control()) {
        return Err(denied(
            whole,
            format!(
                "The name contains the control character U+{:04X}.",
                ch as u32
            ),
            "Use printable characters only.",
        ));
    }
    if c.len() > MAX_COMPONENT_BYTES {
        return Err(denied(
            &elide(c),
            format!(
                "One name in the path is {} bytes, over the {MAX_COMPONENT_BYTES}-byte \
                 limit every filesystem here enforces.",
                c.len()
            ),
            "Shorten it.",
        ));
    }
    if c.ends_with(' ') || c.ends_with('.') {
        // Windows silently strips both, so the file syncs to a machine where
        // it has a different name than the one we recorded — the
        // path-is-never-identity rule's problem arriving through the back door.
        return Err(denied(
            whole,
            "A name ends with a space or a dot, which some filesystems silently strip.",
            "Remove the trailing character.",
        ));
    }

    let stem = c.split('.').next().unwrap_or(c).to_ascii_lowercase();
    if RESERVED_STEMS.contains(&stem.as_str()) {
        return Err(denied(
            whole,
            format!("`{stem}` is a reserved device name and cannot be opened as a file."),
            "Pick another name.",
        ));
    }
    Ok(())
}

/// Whether a string is entirely dots — `.`, `..`, and the longer runs some
/// filesystems also treat as traversal.
fn is_dots(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c == '.')
}

/// Minimal `%XX` decoder. Not a URL parser: it exists only to see whether a
/// name is hiding a separator, so anything malformed is left as written.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Keep an over-long name out of the log line while still identifying it.
fn elide(s: &str) -> String {
    let head: String = s.chars().take(40).collect();
    format!("{head}… ({} bytes)", s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_of(name: &str) -> Code {
        validate(name)
            .map(|_| panic!("`{name}` should have been refused"))
            .unwrap_err()
            .code()
    }

    #[test]
    fn an_ordinary_relative_name_is_accepted() {
        // The guard has to be usable, or it gets bypassed.
        assert_eq!(
            validate("notes/2026/summary.md").unwrap(),
            ["notes", "2026", "summary.md"]
        );
        assert_eq!(validate(".gitignore").unwrap(), [".gitignore"]);
        assert_eq!(
            validate("caf\u{e9} notes.md").unwrap(),
            ["caf\u{e9} notes.md"]
        );
    }

    #[test]
    fn a_name_that_climbs_out_of_the_workspace_is_refused() {
        // The whole attack, in its plainest form: it arrives as data, from a
        // model that read it in a document.
        for bad in [
            "../escape.md",
            "notes/../../escape.md",
            "./../escape.md",
            "a/./../../b",
        ] {
            assert_eq!(code_of(bad), Code::FsPathEscapeBlocked, "`{bad}`");
        }
    }

    #[test]
    fn an_absolute_path_is_refused_even_when_it_points_inside_the_workspace() {
        // The API is workspace-relative. Accepting absolute paths would mean
        // the containment check is the only thing standing between a caller
        // and `/etc`, and one check is not enough for that.
        assert_eq!(code_of("/etc/passwd"), Code::FsPathEscapeBlocked);
        assert_eq!(code_of("/tmp/notes.md"), Code::FsPathEscapeBlocked);
    }

    #[test]
    fn a_percent_escaped_separator_is_refused_even_though_the_kernel_ignores_it() {
        // `..%2f` opens a file literally called `..%2f` on macOS. It is refused
        // because the next consumer of this string — a URL, an archive, a shell
        // — is the one that decodes it.
        for bad in ["..%2fescape.md", "..%2Fescape.md", "%2e%2e/escape.md"] {
            assert_eq!(code_of(bad), Code::FsPathEscapeBlocked, "`{bad}`");
        }
        // A percent sign that decodes to nothing interesting is a legal name.
        assert!(validate("100%25 done.md").is_ok());
    }

    #[test]
    fn a_unicode_character_that_folds_to_a_dot_is_refused() {
        // U+2025 TWO DOT LEADER folds to `..` under NFKC; U+FF0E FULLWIDTH FULL
        // STOP folds to `.`. NFC — which is what path identity uses — leaves
        // both untouched, so this needs its own check.
        for bad in ["\u{2025}/escape.md", "\u{ff0e}\u{ff0e}/escape.md"] {
            assert_eq!(code_of(bad), Code::FsPathEscapeBlocked, "`{bad}`");
        }
    }

    #[test]
    fn a_name_the_filesystem_would_mangle_is_refused_by_policy_not_as_an_escape() {
        // These do not escape anything; they produce a file whose name is not
        // the name we recorded. Different code, because the caller's fix is
        // different: pick another name.
        for bad in [
            "a\0b.md",
            "",
            "   ",
            "notes//x.md",
            "trailing .md ",
            "trailing.",
            "CON.md",
            "com1",
            "nul.txt",
            "bell\u{7}.md",
        ] {
            assert_eq!(code_of(bad), Code::ActNameRejected, "`{bad}`");
        }
    }

    #[test]
    fn a_name_longer_than_the_filesystem_allows_is_refused_before_the_syscall() {
        // Otherwise ENAMETOOLONG reaches the user as "the file could not be
        // read", which is the generic-message defect SUP-001 is about.
        let long_component = "a".repeat(300);
        assert_eq!(code_of(&long_component), Code::ActNameRejected);
        let long_path = vec!["dir"; 400].join("/");
        assert_eq!(code_of(&long_path), Code::ActNameRejected);
        // 255 bytes exactly is legal and must still be accepted.
        assert!(validate(&"a".repeat(255)).is_ok());
    }

    #[test]
    fn every_refusal_message_names_a_cause_and_an_action() {
        // SUP-001. A message that only restates the code teaches nothing at 1am.
        for bad in ["../x", "a\0b", "CON.md", "/etc/passwd", &"a".repeat(300)] {
            let e = validate(bad).unwrap_err();
            assert!(
                e.message().len() > 40 && e.context().is_some(),
                "`{bad}` refused with a bare message: {e}"
            );
        }
    }
}
