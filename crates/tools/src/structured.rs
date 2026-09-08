//! Set a key in a config file through a parser, rather than by matching text.
//!
//! # What this buys over [`crate::patch`]
//!
//! An anchored patch is exact and knows nothing about what it is editing.
//! `find: "8080"` will happily land in a comment, in a different key that
//! happens to share the value, or in the middle of a version string. And
//! because it works on text, it can produce a file that no longer parses —
//! which is why [`crate::validate`] exists to catch that afterwards.
//!
//! Going through the parser removes both problems rather than detecting them.
//! A key is addressed by *where it is*, not by what it looks like; there is no
//! ambiguity to resolve; and the result cannot be syntactically invalid because
//! it was never text in between.
//!
//! # TOML only, and that is a real limit rather than a first instalment
//!
//! **The whole value of this depends on not reformatting the file**, and that
//! is the part most such tools get wrong. `toml_edit` preserves comments, key
//! order, blank lines and whitespace, so setting one key produces a one-line
//! diff.
//!
//! JSON is **refused**, and not because it is harder. There is no
//! format-preserving JSON editor in this tree; `serde_json` would round-trip
//! the document and re-emit it, so changing one key would silently restyle the
//! entire file — a whole-file diff, the author's indentation gone, and any
//! trailing structure they cared about rearranged. That is a worse defect than
//! the one this module fixes, and shipping it while calling it a structural
//! edit would be the more dishonest of the two options. JSON has
//! [`crate::patch`], which does not pretend to understand the file, plus the
//! reparse check that refuses an edit leaving it broken.
//!
//! YAML is not here for the simpler reason that nothing in this tree can parse
//! it at all.
//!
//! # Creating a key is a different request from changing one
//!
//! Setting a key that does not exist is a legitimate thing to want and also
//! exactly how a typo becomes a dead config entry beside the live one it was
//! meant to fix — silently, since both are valid TOML. So `create` is a
//! separate flag and defaults to off, the same way [`crate::Expect::New`] and
//! `Replacing` are separate rather than one permissive default. The lesson is
//! the one `undo_write` learned the hard way: when two operations differ in
//! what they destroy, the caller says which, and "neither" is a refusal.

use marrow_core::{Code, Error, Result};
use serde::{Deserialize, Serialize};
use toml_edit::{Document, Item, Value};

use crate::guard::{Expect, Workspace, Written};

/// Set one key in a TOML file.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SetValue {
    /// Workspace-relative path. Must end `.toml`.
    pub path: String,
    /// Dotted key path — `server.port`, or `tool."my crate".version` when a
    /// segment contains a dot or a space.
    pub key: String,
    /// The new value **as TOML**: `8080`, `"localhost"`, `true`, `[1, 2]`.
    ///
    /// Typed rather than stringly: `port = 8080` and `port = "8080"` are
    /// different files and a caller that meant one should not get the other by
    /// accident. Quote it if you want a string.
    pub value: String,
    /// Allow creating a key that is not there. Off by default — see the module
    /// docs.
    #[serde(default)]
    pub create: bool,
    /// What the caller read. Required, as for a patch.
    pub expect: Expect,
}

/// Apply it.
pub fn set_value(ws: &Workspace, req: &SetValue) -> Result<Written> {
    let Expect::Replacing(expected) = &req.expect else {
        return Err(Error::new(
            Code::CfgInvalid,
            "Setting a key edits a file that exists, so it needs the digest of what you \
             read. Read the file first and pass it as `expect`.",
        )
        .with_context(req.path.clone()));
    };

    if !req.path.to_ascii_lowercase().ends_with(".toml") {
        return Err(Error::new(
            Code::ParUnsupported,
            "Only TOML can be edited through its parser here. For JSON use the anchored \
             patch: there is no format-preserving JSON editor available, and re-emitting \
             the document would restyle the whole file to change one key.",
        )
        .with_context(req.path.clone()));
    }

    let segments = split_key(&req.key)?;
    let text = crate::patch::read_text_for_edit(ws, &req.path, expected)?;

    let mut doc = text.parse::<Document>().map_err(|e| {
        Error::new(
            Code::ParCorrupt,
            "That file is not valid TOML, so a key cannot be set in it through the parser. \
             Fix the syntax first, or use the anchored patch, which does not need to \
             understand the file.",
        )
        .with_context(format!("{}: {e}", req.path))
    })?;

    let new_value: Value = req.value.parse::<Value>().map_err(|e| {
        Error::new(
            Code::CfgInvalid,
            "`value` is not a TOML value. Write it as it would appear in the file — \
             `8080`, `\"localhost\"`, `true`, `[1, 2]` — and quote it if it is a string.",
        )
        .with_context(format!("{}: {e}", req.value))
    })?;

    let (parents, leaf) = segments.split_at(segments.len() - 1);
    let leaf = &leaf[0];

    // Walk to the parent table. A missing intermediate is refused even under
    // `create`: conjuring `[server]` because somebody misspelled `[sever]` is
    // the failure this is supposed to prevent, not a convenience.
    let mut table = doc.as_table_mut() as &mut dyn toml_edit::TableLike;
    for seg in parents {
        let next = table.get_mut(seg).ok_or_else(|| missing(&req.key, seg))?;
        table = next.as_table_like_mut().ok_or_else(|| {
            Error::new(
                Code::CfgInvalid,
                format!(
                    "`{seg}` in that key is a value, not a table, so nothing can be set inside it."
                ),
            )
            .with_context(req.key.clone())
        })?;
    }

    let existed = table.get(leaf).is_some();
    if !existed && !req.create {
        return Err(Error::new(
            Code::FsNotFound,
            format!(
                "There is no `{}` in that file. If you meant to add it, ask for that \
                 explicitly — a key that is merely misspelled would otherwise be created \
                 beside the one you meant to change, and both are valid TOML.",
                req.key
            ),
        )
        .with_context(req.path.clone()));
    }

    if let Some(current) = table.get(leaf) {
        if current.as_value().map(|v| v.to_string().trim().to_string())
            == Some(new_value.to_string().trim().to_string())
        {
            return Err(Error::new(
                Code::CfgInvalid,
                "That key already has that value, so this would rewrite the file without \
                 changing it. Nothing was written.",
            )
            .with_context(format!("{} = {}", req.key, req.value)));
        }
    }

    // **Replace the value, not the entry.** `insert` drops the existing key's
    // decor, so `port  =  8080` came back as `port = 9090`: everything else in
    // the file survived and the edited line lost its own alignment, which is
    // the one line somebody is looking at. Carrying the old value's decor over
    // keeps the spacing either side of it — and any trailing comment, which
    // lives in that same suffix.
    match table.get_mut(leaf) {
        Some(item) if item.is_value() => {
            let decor = item
                .as_value()
                .expect("checked by the guard above")
                .decor()
                .clone();
            let mut v = new_value;
            *v.decor_mut() = decor;
            *item = Item::Value(v);
        }
        // A key that is not there, or is a table being replaced by a value.
        // Nothing to carry over, so a plain insert is right.
        _ => {
            table.insert(leaf, Item::Value(new_value));
        }
    }

    let updated = doc.to_string();
    if updated == text {
        return Err(Error::invariant(
            "a set that changed a key produced identical bytes",
        ));
    }

    ws.write(&req.path, updated.as_bytes(), &req.expect)
}

fn missing(key: &str, seg: &str) -> Error {
    Error::new(
        Code::FsNotFound,
        format!("`{seg}` is not in that file, so `{key}` has nowhere to go."),
    )
}

/// Split `a.b."c d"` into its segments.
///
/// Quoted segments exist because TOML keys may contain dots and spaces, and a
/// naive `split('.')` would address the wrong thing without saying so.
fn split_key(key: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = key.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '.' if !quoted => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
        let _ = chars.peek();
    }
    if quoted {
        return Err(
            Error::new(Code::CfgInvalid, "That key has an unclosed quote in it.")
                .with_context(key.to_string()),
        );
    }
    out.push(cur);

    if out.iter().any(|s| s.is_empty()) {
        return Err(Error::new(
            Code::CfgInvalid,
            "That key has an empty segment — a leading, trailing or doubled dot.",
        )
        .with_context(key.to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_core::ContentHash;

    struct Fixture {
        _tmp: tempfile::TempDir,
        ws: Workspace,
        root: std::path::PathBuf,
    }

    fn fixture(name: &str, body: &str) -> (Fixture, ContentHash) {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join(name), body).unwrap();
        let ws = Workspace::open(&root).unwrap();
        (
            Fixture {
                _tmp: tmp,
                ws,
                root,
            },
            ContentHash::of(body.as_bytes()),
        )
    }

    fn req(path: &str, key: &str, value: &str, d: ContentHash) -> SetValue {
        SetValue {
            path: path.into(),
            key: key.into(),
            value: value.into(),
            create: false,
            expect: Expect::Replacing(d),
        }
    }

    /// The property the whole module is for. A tool that reformats the file is
    /// worse than one that never understood it.
    #[test]
    fn everything_except_the_one_key_survives_byte_for_byte() {
        let body = "\
# The port the server listens on.
# Changed 2024-01-01 after the incident.
[server]
host  =  \"localhost\"   # deliberately padded
port  =  8080

# A trailing comment, and a blank line after it.

[other]
keep = true
";
        let (f, d) = fixture("c.toml", body);
        set_value(&f.ws, &req("c.toml", "server.port", "9090", d)).expect("sets");

        let after = std::fs::read_to_string(f.root.join("c.toml")).unwrap();
        assert_eq!(
            after,
            body.replace("port  =  8080", "port  =  9090"),
            "only the value changed"
        );
        assert!(after.contains("# Changed 2024-01-01 after the incident."));
        assert!(after.contains("host  =  \"localhost\"   # deliberately padded"));
    }

    #[test]
    fn a_nested_key_is_addressed_by_path() {
        let body = "[a.b]\nc = 1\n";
        let (f, d) = fixture("c.toml", body);
        set_value(&f.ws, &req("c.toml", "a.b.c", "2", d)).expect("sets");
        assert_eq!(
            std::fs::read_to_string(f.root.join("c.toml")).unwrap(),
            "[a.b]\nc = 2\n"
        );
    }

    #[test]
    fn a_quoted_segment_may_contain_a_dot_or_a_space() {
        let body = "[tool.\"my crate\"]\nversion = \"1.0\"\n";
        let (f, d) = fixture("c.toml", body);
        set_value(
            &f.ws,
            &req("c.toml", "tool.\"my crate\".version", "\"2.0\"", d),
        )
        .expect("sets");
        assert!(std::fs::read_to_string(f.root.join("c.toml"))
            .unwrap()
            .contains("version = \"2.0\""));
    }

    /// The reason `create` exists. Both spellings are valid TOML, so a
    /// misspelling would sit beside the live key doing nothing, forever.
    #[test]
    fn a_key_that_is_not_there_is_refused_rather_than_created() {
        let body = "[server]\nport = 8080\n";
        let (f, d) = fixture("c.toml", body);
        let e =
            set_value(&f.ws, &req("c.toml", "server.prot", "9090", d)).expect_err("must refuse");
        assert_eq!(e.code(), Code::FsNotFound);
        assert_eq!(
            std::fs::read_to_string(f.root.join("c.toml")).unwrap(),
            body
        );
    }

    #[test]
    fn creating_a_key_works_when_it_is_asked_for_explicitly() {
        let body = "[server]\nport = 8080\n";
        let (f, d) = fixture("c.toml", body);
        let mut r = req("c.toml", "server.timeout", "30", d);
        r.create = true;
        set_value(&f.ws, &r).expect("creates");
        let after = std::fs::read_to_string(f.root.join("c.toml")).unwrap();
        assert!(after.contains("timeout = 30"));
        assert!(after.contains("port = 8080"), "and the rest is untouched");
    }

    /// Even under `create`. Conjuring `[sever]` because somebody misspelled
    /// `[server]` is the failure this is meant to prevent.
    #[test]
    fn a_missing_intermediate_table_is_never_conjured() {
        let body = "[server]\nport = 8080\n";
        let (f, d) = fixture("c.toml", body);
        let mut r = req("c.toml", "sever.port", "9090", d);
        r.create = true;
        let e = set_value(&f.ws, &r).expect_err("must refuse");
        assert_eq!(e.code(), Code::FsNotFound);
        assert_eq!(
            std::fs::read_to_string(f.root.join("c.toml")).unwrap(),
            body
        );
    }

    #[test]
    fn the_value_is_typed_so_a_number_does_not_become_a_string() {
        let body = "[a]\nn = 1\n";
        let (f, d) = fixture("c.toml", body);
        set_value(&f.ws, &req("c.toml", "a.n", "\"2\"", d)).expect("sets");
        assert!(std::fs::read_to_string(f.root.join("c.toml"))
            .unwrap()
            .contains("n = \"2\""));
    }

    #[test]
    fn a_value_that_is_not_toml_is_refused() {
        let (f, d) = fixture("c.toml", "[a]\nn = 1\n");
        let e = set_value(&f.ws, &req("c.toml", "a.n", "not valid = toml", d))
            .expect_err("must refuse");
        assert_eq!(e.code(), Code::CfgInvalid);
    }

    #[test]
    fn setting_a_key_to_what_it_already_holds_is_refused_rather_than_rewriting() {
        let (f, d) = fixture("c.toml", "[a]\nn = 1\n");
        let e = set_value(&f.ws, &req("c.toml", "a.n", "1", d)).expect_err("must refuse");
        assert_eq!(e.code(), Code::CfgInvalid);
    }

    #[test]
    fn json_is_refused_and_says_what_to_use_instead() {
        let (f, d) = fixture("c.json", "{\"a\": 1}");
        let e = set_value(&f.ws, &req("c.json", "a", "2", d)).expect_err("must refuse");
        assert_eq!(e.code(), Code::ParUnsupported);
        assert!(e.message().contains("patch"), "{}", e.message());
    }

    #[test]
    fn a_file_that_is_not_valid_toml_says_so_rather_than_guessing() {
        let (f, d) = fixture("c.toml", "[a\nn = 1\n");
        let e = set_value(&f.ws, &req("c.toml", "a.n", "2", d)).expect_err("must refuse");
        assert_eq!(e.code(), Code::ParCorrupt);
    }

    #[test]
    fn a_stale_digest_is_refused() {
        let (f, _) = fixture("c.toml", "[a]\nn = 1\n");
        let stale = ContentHash::of(b"[a]\nn = 0\n");
        let e = set_value(&f.ws, &req("c.toml", "a.n", "2", stale)).expect_err("must refuse");
        assert_eq!(e.code(), Code::ActStaleVersion);
    }

    #[test]
    fn a_patch_with_no_digest_is_refused() {
        let (f, _) = fixture("c.toml", "[a]\nn = 1\n");
        let mut r = req("c.toml", "a.n", "2", ContentHash::of(b""));
        r.expect = Expect::New;
        let e = set_value(&f.ws, &r).expect_err("must refuse");
        assert_eq!(e.code(), Code::CfgInvalid);
    }

    #[test]
    fn malformed_key_paths_are_refused() {
        for bad in ["a..b", ".a", "a.", "a.\"b"] {
            assert!(split_key(bad).is_err(), "`{bad}` should be refused");
        }
        assert_eq!(split_key("a.b").unwrap(), vec!["a", "b"]);
        assert_eq!(split_key("a.\"b.c\".d").unwrap(), vec!["a", "b.c", "d"]);
    }

    #[test]
    fn a_set_cannot_reach_outside_the_workspace() {
        let (f, d) = fixture("c.toml", "[a]\nn = 1\n");
        let e = set_value(&f.ws, &req("../escaped.toml", "a.n", "2", d)).expect_err("must refuse");
        assert!(
            matches!(e.code(), Code::FsPathEscapeBlocked | Code::ActNameRejected),
            "unexpected {:?}",
            e.code()
        );
    }
}
