//! Refuse an edit that would break a file it did not break.
//!
//! # The rule, and why it is a regression check rather than a validity check
//!
//! **A write is refused only if the file parsed before and does not parse
//! after.** Not "the result must be valid" — that rule would refuse the one
//! edit most worth allowing, which is the one repairing a file that is already
//! broken. Somebody whose `tsconfig.json` has a trailing comma cannot be told
//! that they may not fix it because it does not currently parse.
//!
//! So validity is a property that may not be *lost*, never one that must be
//! achieved. A file that was broken and stays broken is not this module's
//! business; the caller asked for an edit and got one.
//!
//! # Why this is worth having at all
//!
//! An anchored replacement is exact, and being exact is not the same as being
//! right. `find: "8080"` in a config file replaces a port; the same anchor in
//! `{"port": 8080}` where the caller meant `"timeout": 8080` produces perfectly
//! well-formed JSON with the wrong meaning — nothing here catches that, and
//! nothing can. What it *does* catch is the other kind: removing a line that
//! happened to carry the closing brace, replacing a quoted string with one that
//! contains an unescaped quote, deleting a `]` along with the last array
//! element. Those turn a file that a program reads at startup into one that
//! stops it starting, and the damage shows up somewhere else entirely, hours
//! later.
//!
//! # What is covered, and what is deliberately not
//!
//! JSON and TOML. Both parsers are already dependencies, and both are formats
//! where "no longer loads" is a silent catastrophe in a file nobody re-reads
//! until something fails.
//!
//! **Not YAML**, because there is no YAML parser in this tree and adding one to
//! validate a format the corpus barely contains is building ahead. **Not code**,
//! because Tree-sitter is an error-tolerant parser: it produces a tree for
//! almost anything, so "it parsed" would be nearly meaningless, and counting
//! `ERROR` nodes before and after is a real design rather than a check to bolt
//! on here. **Not Markdown**, which has no invalid form to detect.
//!
//! A format not listed is not validated, and that is reported as such rather
//! than as a pass — [`Checked::Unvalidated`] exists so the difference between
//! "this is fine" and "nothing looked" survives to the caller.

use std::path::Path;

use marrow_core::{Code, Error, Result};

/// A format whose structural validity can be checked cheaply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Toml,
}

impl Format {
    /// By extension. The bytes decide what a *parser* does elsewhere in this
    /// project (FS-014), but here the extension is the right question: the
    /// claim being checked is "this file still is what it is named as", and a
    /// `.json` that never contained JSON simply fails the before-check and is
    /// left alone.
    pub fn of_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "json" => Some(Format::Json),
            "toml" => Some(Format::Toml),
            _ => None,
        }
    }

    fn parses(self, text: &str) -> std::result::Result<(), String> {
        match self {
            Format::Json => serde_json::from_str::<serde_json::Value>(text)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Format::Toml => toml::from_str::<toml::Value>(text)
                .map(|_| ())
                .map_err(|e| e.to_string()),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Json => "JSON",
            Format::Toml => "TOML",
        }
    }
}

/// What the check actually did. Three outcomes, not two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checked {
    /// Parsed before and after, or was repaired by this edit.
    Held,
    /// No validator for this format. **Not the same as passing**, and kept
    /// distinct so a caller reporting "validated" cannot be quietly wrong.
    Unvalidated,
    /// Did not parse before either. The edit is allowed through: a file that
    /// was already broken is not this edit's fault, and refusing here would
    /// block the repair.
    WasAlreadyBroken,
}

/// Refuse `after` if `before` parsed and it does not.
///
/// The error names the format and quotes the parser, because "invalid JSON" with
/// no position in it is the generic failure text this project treats as a
/// defect — the caller needs to know *where* its anchor did the damage.
pub fn check_no_regression(path: &Path, before: &str, after: &str) -> Result<Checked> {
    let Some(format) = Format::of_path(path) else {
        return Ok(Checked::Unvalidated);
    };

    if format.parses(before).is_err() {
        return Ok(Checked::WasAlreadyBroken);
    }

    match format.parses(after) {
        Ok(()) => Ok(Checked::Held),
        Err(why) => Err(Error::new(
            Code::ActValidationFailed,
            format!(
                "This edit would leave the file invalid {}, and it parsed before. Nothing \
                 was written. The anchor probably took a bracket, a quote or a comma with \
                 it.",
                match format {
                    Format::Json => "JSON",
                    Format::Toml => "TOML",
                }
            ),
        )
        .with_context(format!("{}: {why}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    #[test]
    fn an_edit_that_keeps_json_valid_is_allowed() {
        let r =
            check_no_regression(&p("a.json"), r#"{"port": 8080}"#, r#"{"port": 9090}"#).unwrap();
        assert_eq!(r, Checked::Held);
    }

    /// The case this exists for: an anchor that took the closing brace with it.
    #[test]
    fn an_edit_that_breaks_json_is_refused_and_says_where() {
        let e = check_no_regression(&p("a.json"), r#"{"port": 8080}"#, r#"{"port": 8080"#)
            .expect_err("must refuse");
        assert_eq!(e.code(), Code::ActValidationFailed);
        let ctx = format!("{e:?}");
        assert!(
            ctx.contains("line") || ctx.contains("column") || ctx.contains("EOF"),
            "the parser's own words must survive: {ctx}"
        );
    }

    /// The rule that keeps this from being worse than nothing. Somebody whose
    /// config has a trailing comma must be able to fix it.
    #[test]
    fn a_file_that_was_already_broken_can_still_be_edited() {
        let r =
            check_no_regression(&p("a.json"), r#"{"port": 8080,}"#, r#"{"port": 8080,,}"#).unwrap();
        assert_eq!(
            r,
            Checked::WasAlreadyBroken,
            "a broken file is not this edit's fault"
        );
    }

    #[test]
    fn repairing_a_broken_file_is_allowed_and_reported_as_such() {
        let r =
            check_no_regression(&p("a.json"), r#"{"port": 8080,}"#, r#"{"port": 8080}"#).unwrap();
        assert_eq!(r, Checked::WasAlreadyBroken);
    }

    #[test]
    fn toml_is_checked_too() {
        assert_eq!(
            check_no_regression(&p("c.toml"), "a = 1\n", "a = 2\n").unwrap(),
            Checked::Held
        );
        let e = check_no_regression(&p("c.toml"), "a = 1\n", "a = \n").expect_err("must refuse");
        assert_eq!(e.code(), Code::ActValidationFailed);
    }

    /// "Nothing looked" must not read as "this is fine".
    #[test]
    fn a_format_with_no_validator_says_so_rather_than_passing() {
        assert_eq!(
            check_no_regression(&p("notes.md"), "# a", "# b").unwrap(),
            Checked::Unvalidated
        );
        assert_eq!(
            check_no_regression(&p("config.yaml"), "a: 1", "a: [").unwrap(),
            Checked::Unvalidated,
            "YAML is not covered, and says so rather than passing silently"
        );
        assert_eq!(
            check_no_regression(&p("noext"), "x", "y").unwrap(),
            Checked::Unvalidated
        );
    }

    #[test]
    fn the_extension_is_matched_case_blind() {
        assert_eq!(Format::of_path(&p("A.JSON")), Some(Format::Json));
        assert_eq!(Format::of_path(&p("C.Toml")), Some(Format::Toml));
    }

    /// A `.json` that never held JSON fails the before-check and is left alone,
    /// so misnaming a file does not lock it.
    #[test]
    fn a_file_named_json_that_never_was_json_is_not_locked_by_this() {
        let r = check_no_regression(&p("a.json"), "not json at all", "still not json").unwrap();
        assert_eq!(r, Checked::WasAlreadyBroken);
    }
}
