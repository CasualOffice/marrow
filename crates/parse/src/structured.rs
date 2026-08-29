//! TOML, JSON and YAML (T1). ~165 files — M0 §6 priority 4.
//!
//! One [`IrKind::KeyValue`] node per entry, carrying its dotted key path and
//! the byte range of `key: value` in the file. Nesting is followed to
//! [`crate::budget::Budgets::max_structured_depth`] and then stops: the value
//! of a deeply nested key is still covered by its ancestor's span, so nothing
//! becomes uncitable — it just gets cited at a coarser grain.
//!
//! # Where the spec's model met three real libraries
//!
//! "Preserve key paths; a node per top-level table/object with its byte range"
//! assumes the parser hands back positions. Exactly one of the three does:
//!
//! - **TOML** — `toml_edit` keeps a span on every key and value. Exact.
//! - **JSON** — no Rust JSON parser in common use reports offsets (`serde_json`
//!   deserialises and discards them). So `serde_json` validates the document
//!   and a small structural scanner in this module produces the spans. Two
//!   passes, but the second is the only one that can satisfy invariant #1.
//! - **YAML** — `yaml-rust2` validates; an indentation scanner locates the
//!   keys. The marked-event API could do it in one pass, but it reports a start
//!   marker per event and no end, so the ends would be reconstructed from the
//!   next event's start — which is what the line scanner does anyway, with far
//!   less machinery. Flow-style mappings (`{a: 1}` on one line) yield no key
//!   nodes and are reported as `Partial`.

use std::ops::Range;

use marrow_core::{Code, Error, Result};

use crate::decode;
use crate::ir::{
    ArtifactBuilder, IrKind, IrNode, LineIndex, NodeAttrs, ParseOutcome, ParseWarning,
    ParsedArtifact, ParserTier,
};
use crate::parser::{ContentParser, FileProbe, ParseInput};

/// Which structured format a file claims to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Toml,
    Json,
    Yaml,
}

impl Format {
    pub fn from_probe(probe: &FileProbe) -> Option<Self> {
        // `Cargo.lock` and `.lock` siblings are TOML with a name instead of an
        // extension; M0 found them and they hold real dependency facts.
        if probe.file_name.eq_ignore_ascii_case("Cargo.lock") {
            return Some(Format::Toml);
        }
        Some(match probe.extension.as_deref()? {
            "toml" => Format::Toml,
            "json" => Format::Json,
            "yaml" | "yml" => Format::Yaml,
            _ => return None,
        })
    }
}

/// One extracted entry, before it becomes a node.
#[derive(Debug)]
struct Entry {
    key_path: String,
    name: String,
    range: Range<usize>,
    /// Index into the entry list, not the arena.
    parent: Option<usize>,
}

/// The T1 structured-config parser.
#[derive(Clone, Copy, Debug, Default)]
pub struct StructuredParser;

impl StructuredParser {
    pub const ID: &'static str = "structured";
    pub const VERSION: &'static str = "1";
}

impl ContentParser for StructuredParser {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn version(&self) -> &'static str {
        Self::VERSION
    }

    fn tier(&self) -> ParserTier {
        ParserTier::T1
    }

    fn handles(&self, probe: &FileProbe) -> bool {
        Format::from_probe(probe).is_some()
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedArtifact> {
        let format = Format::from_probe(input.probe).ok_or_else(|| {
            Error::new(Code::ParUnsupported, "Not a structured configuration file.")
        })?;
        let decoded = decode::decode(input.bytes)?;
        let src = decoded.text.as_str();
        if src.trim().is_empty() {
            return Err(Error::new(
                Code::ParLowYield,
                "This configuration file is empty, so only its metadata is indexed.",
            ));
        }

        let mut b = ArtifactBuilder::new(Self::ID, Self::VERSION, self.tier(), input.budget);
        b.degrade_provenance(decoded.provenance_ceiling());
        let max_depth = b.budget().limits().max_structured_depth;

        let entries = match format {
            Format::Toml => toml_entries(src, max_depth)?,
            Format::Json => json_entries(src, max_depth)?,
            Format::Yaml => yaml_entries(src, max_depth)?,
        };

        if entries.is_empty() {
            return Err(Error::new(
                Code::ParLowYield,
                "This configuration file has no key/value entries, so it is indexed as plain \
                 text instead.",
            ));
        }

        let lines = LineIndex::new(src);
        // Entry index -> arena index. Entries are emitted in document order, so
        // a parent is always already present.
        let mut arena: Vec<usize> = Vec::with_capacity(entries.len());
        for e in &entries {
            let parent = e.parent.and_then(|p| arena.get(p).copied());
            let (text, clipped) = b
                .budget()
                .clamp_text(src.get(e.range.clone()).unwrap_or(""));
            let node = IrNode::content_in(IrKind::KeyValue, src, e.range.clone(), text)?
                .with_attrs(NodeAttrs {
                    key_path: Some(e.key_path.clone()),
                    name: Some(e.name.clone()),
                    language: Some(format_label(format).to_owned()),
                    ..NodeAttrs::default().with_lines(&lines, &e.range)
                });
            arena.push(b.push(parent, node)?);
            if clipped {
                b.warn(ParseWarning::new(
                    Code::ParTruncated,
                    "A configuration value was larger than the per-node text budget and was \
                     clipped. Its byte span still covers the whole value.",
                ));
                b.set_outcome(ParseOutcome::Partial);
            }
        }

        Ok(b.finish())
    }
}

const fn format_label(f: Format) -> &'static str {
    match f {
        Format::Toml => "toml",
        Format::Json => "json",
        Format::Yaml => "yaml",
    }
}

// ---------------------------------------------------------------- TOML

fn toml_entries(src: &str, max_depth: u16) -> Result<Vec<Entry>> {
    let doc = toml_edit::Document::parse(src).map_err(|e| {
        let mut err = Error::new(
            Code::ParCorrupt,
            "This TOML file could not be parsed. Fix the syntax error and it will be \
             re-indexed automatically.",
        )
        .with_context(e.to_string());
        if let Some(span) = e.span() {
            err = err.with_context(format!("bytes {}..{}: {e}", span.start, span.end));
        }
        err
    })?;

    let mut out = Vec::new();
    walk_toml(doc.as_table(), "", None, 0, max_depth, &mut out);
    Ok(out)
}

fn walk_toml(
    table: &toml_edit::Table,
    prefix: &str,
    parent: Option<usize>,
    depth: u16,
    max_depth: u16,
    out: &mut Vec<Entry>,
) {
    for (name, _) in table.iter() {
        let Some((key, item)) = table.get_key_value(name) else {
            continue;
        };
        let key_path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}.{name}")
        };

        // An implicit parent table (`[a.b]` with no `[a]`) has no span. There
        // is nothing in the file to point at, so no node is emitted — but the
        // walk continues, because `a.b` does exist in the file.
        let range = span_union(key.span(), item.span());
        let idx = range.map(|range| {
            out.push(Entry {
                key_path: key_path.clone(),
                name: name.to_owned(),
                range,
                parent,
            });
            out.len() - 1
        });

        if depth + 1 >= max_depth {
            continue;
        }
        match item {
            toml_edit::Item::Table(t) => {
                walk_toml(t, &key_path, idx.or(parent), depth + 1, max_depth, out)
            }
            toml_edit::Item::ArrayOfTables(arr) => {
                for (i, t) in arr.iter().enumerate() {
                    walk_toml(
                        t,
                        &format!("{key_path}[{i}]"),
                        idx.or(parent),
                        depth + 1,
                        max_depth,
                        out,
                    );
                }
            }
            _ => {}
        }
    }
}

fn span_union(a: Option<Range<usize>>, b: Option<Range<usize>>) -> Option<Range<usize>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.start.min(b.start)..a.end.max(b.end)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

// ---------------------------------------------------------------- JSON

fn json_entries(src: &str, max_depth: u16) -> Result<Vec<Entry>> {
    // Validation first, by a parser nobody has to trust me about.
    serde_json::from_str::<serde_json::Value>(src).map_err(|e| {
        Error::new(
            Code::ParCorrupt,
            "This JSON file could not be parsed. Fix the syntax error and it will be \
             re-indexed automatically.",
        )
        .with_context(format!("line {} column {}: {e}", e.line(), e.column()))
    })?;

    let mut scan = Json {
        b: src.as_bytes(),
        i: 0,
    };
    let mut out = Vec::new();
    scan.ws();
    scan.value("", None, 0, max_depth, &mut out)?;
    Ok(out)
}

/// A structural scanner: it locates members, it does not interpret them.
///
/// `serde_json` has already proved the document is well formed by the time this
/// runs, so the error paths here are defence against a disagreement between the
/// two, not against hostile input.
struct Json<'a> {
    b: &'a [u8],
    i: usize,
}

impl Json<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn malformed() -> Error {
        Error::new(
            Code::ParCorrupt,
            "This JSON file is structured in a way the position scanner could not follow, so \
             it is indexed by metadata only.",
        )
    }

    /// Consume one value, returning its byte range, recording members.
    fn value(
        &mut self,
        path: &str,
        parent: Option<usize>,
        depth: u16,
        max_depth: u16,
        out: &mut Vec<Entry>,
    ) -> Result<Range<usize>> {
        self.ws();
        let start = self.i;
        match self.peek().ok_or_else(Json::malformed)? {
            b'{' => {
                self.i += 1;
                loop {
                    self.ws();
                    match self.peek().ok_or_else(Json::malformed)? {
                        b'}' => {
                            self.i += 1;
                            break;
                        }
                        b',' => {
                            self.i += 1;
                            continue;
                        }
                        b'"' => {}
                        _ => return Err(Json::malformed()),
                    }
                    let key_start = self.i;
                    let key_range = self.string()?;
                    let name = unescape(&self.b[key_range.start + 1..key_range.end - 1]);
                    self.ws();
                    if self.peek() != Some(b':') {
                        return Err(Json::malformed());
                    }
                    self.i += 1;

                    let key_path = if path.is_empty() {
                        name.clone()
                    } else {
                        format!("{path}.{name}")
                    };
                    let record = depth < max_depth;
                    let idx = if record {
                        out.push(Entry {
                            key_path: key_path.clone(),
                            name,
                            // Filled in once the value has been consumed.
                            range: key_start..key_start,
                            parent,
                        });
                        Some(out.len() - 1)
                    } else {
                        None
                    };
                    let value = self.value(&key_path, idx.or(parent), depth + 1, max_depth, out)?;
                    if let Some(i) = idx {
                        out[i].range = key_start..value.end;
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut n = 0usize;
                loop {
                    self.ws();
                    match self.peek().ok_or_else(Json::malformed)? {
                        b']' => {
                            self.i += 1;
                            break;
                        }
                        b',' => {
                            self.i += 1;
                            continue;
                        }
                        _ => {}
                    }
                    let key_path = format!("{path}[{n}]");
                    let element_start = self.i;
                    let record = depth < max_depth;
                    let idx = if record {
                        out.push(Entry {
                            key_path: key_path.clone(),
                            name: n.to_string(),
                            range: element_start..element_start,
                            parent,
                        });
                        Some(out.len() - 1)
                    } else {
                        None
                    };
                    let value = self.value(&key_path, idx.or(parent), depth + 1, max_depth, out)?;
                    if let Some(i) = idx {
                        out[i].range = element_start..value.end;
                    }
                    n += 1;
                }
            }
            b'"' => {
                self.string()?;
            }
            _ => {
                // Number, `true`, `false`, `null`. Consume to the next
                // structural character.
                while let Some(c) = self.peek() {
                    if c.is_ascii_whitespace() || matches!(c, b',' | b'}' | b']') {
                        break;
                    }
                    self.i += 1;
                }
                if self.i == start {
                    return Err(Json::malformed());
                }
            }
        }
        Ok(start..self.i)
    }

    /// Consume a quoted string, returning its range **including** the quotes.
    fn string(&mut self) -> Result<Range<usize>> {
        let start = self.i;
        if self.peek() != Some(b'"') {
            return Err(Json::malformed());
        }
        self.i += 1;
        loop {
            match self.peek().ok_or_else(Json::malformed)? {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    return Ok(start..self.i);
                }
                _ => self.i += 1,
            }
        }
    }
}

/// Minimal JSON string unescaping, for key names only.
///
/// Key names become `key_path`, which is compared and displayed; values keep
/// their raw slice, so nothing here has to round-trip.
fn unescape(bytes: &[u8]) -> String {
    let raw = String::from_utf8_lossy(bytes);
    if !raw.contains('\\') {
        return raw.into_owned();
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => out.push(char::REPLACEMENT_CHARACTER),
                }
            }
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

// ---------------------------------------------------------------- YAML

fn yaml_entries(src: &str, max_depth: u16) -> Result<Vec<Entry>> {
    yaml_rust2::YamlLoader::load_from_str(src).map_err(|e| {
        Error::new(
            Code::ParCorrupt,
            "This YAML file could not be parsed. Fix the syntax error and it will be \
             re-indexed automatically.",
        )
        .with_context(e.to_string())
    })?;

    struct Level {
        indent: usize,
        entry: usize,
    }

    let mut out: Vec<Entry> = Vec::new();
    let mut stack: Vec<Level> = Vec::new();
    // Set when a key's value is a block scalar (`|` / `>`): everything more
    // indented is literal text, not keys.
    let mut literal_above: Option<usize> = None;
    let mut offset = 0usize;

    for line in src.split_inclusive('\n') {
        let start = offset;
        offset += line.len();

        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let body = trimmed.trim_end();

        if let Some(limit) = literal_above {
            if !body.is_empty() && indent <= limit {
                literal_above = None;
            } else {
                continue;
            }
        }
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        if body == "---" || body == "..." {
            stack.clear();
            continue;
        }
        // Sequence items are located through their parent key's span; a
        // dedicated node per element would need a synthetic key.
        if body.starts_with("- ") || body == "-" {
            continue;
        }
        let Some(colon) = key_colon(body) else {
            continue;
        };
        let name = body[..colon].trim().trim_matches(['"', '\'']).to_owned();
        if name.is_empty() {
            continue;
        }

        while stack.last().is_some_and(|l| l.indent >= indent) {
            stack.pop();
        }
        let parent = stack.last().map(|l| l.entry);
        let key_path = match parent {
            Some(p) => format!("{}.{name}", out[p].key_path),
            None => name.clone(),
        };

        if stack.len() as u16 >= max_depth {
            // Too deep to name individually; the ancestor's span covers it.
            if body[colon + 1..].trim_start().starts_with(['|', '>']) {
                literal_above = Some(indent);
            }
            continue;
        }

        out.push(Entry {
            key_path,
            name,
            range: start + indent..offset,
            parent,
        });
        stack.push(Level {
            indent,
            entry: out.len() - 1,
        });
        if body[colon + 1..].trim_start().starts_with(['|', '>']) {
            literal_above = Some(indent);
        }
    }

    // A mapping key owns everything under it, so its span runs to the start of
    // the next entry that is *not* one of its descendants. Entries are already
    // in document order, so descendants are exactly the run that follows.
    for i in 0..out.len() {
        let end = ((i + 1)..out.len())
            .find(|j| !descends_from(&out, *j, i))
            .map_or(src.len(), |j| out[j].range.start);
        out[i].range.end = trim_end_of(src, out[i].range.start, end);
    }
    Ok(out)
}

fn descends_from(entries: &[Entry], j: usize, i: usize) -> bool {
    let mut cur = entries[j].parent;
    while let Some(c) = cur {
        if c == i {
            return true;
        }
        cur = entries[c].parent;
    }
    false
}

fn trim_end_of(src: &str, start: usize, end: usize) -> usize {
    let end = end.min(src.len()).max(start);
    match src.get(start..end) {
        Some(s) => start + s.trim_end().len(),
        None => end,
    }
}

/// Find the `:` that separates a YAML key from its value, ignoring colons
/// inside quotes. Returns `None` for a line that is not a mapping entry.
fn key_colon(body: &str) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &c) in bytes.iter().enumerate() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b':' => {
                    let next = bytes.get(i + 1).copied();
                    if next.is_none() || next == Some(b' ') || next == Some(b'\t') {
                        return Some(i);
                    }
                }
                b'#' => return None,
                _ => {}
            },
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetGuard, Budgets};

    fn parse(name: &str, src: &str) -> Result<ParsedArtifact> {
        let probe = FileProbe::new(name, src.len() as u64);
        StructuredParser.parse(ParseInput {
            bytes: src.as_bytes(),
            probe: &probe,
            budget: BudgetGuard::new(Budgets::default()),
        })
    }

    fn paths(a: &ParsedArtifact) -> Vec<String> {
        a.nodes
            .iter()
            .filter_map(|n| n.attrs.key_path.clone())
            .collect()
    }

    #[test]
    fn structured_parser_reads_toml_key_paths_and_spans() {
        let src = "\
name = \"marrow\"

[package]
edition = \"2021\"

[dependencies.serde]
version = \"1\"
";
        let a = parse("Cargo.toml", src).unwrap();
        a.validate().unwrap();
        let p = paths(&a);
        assert!(p.contains(&"name".to_string()));
        assert!(p.contains(&"package".to_string()));
        assert!(p.contains(&"package.edition".to_string()));
        assert!(p.contains(&"dependencies.serde.version".to_string()));

        let name = a
            .nodes
            .iter()
            .find(|n| n.attrs.key_path.as_deref() == Some("name"))
            .unwrap();
        let r = name.byte_range().unwrap();
        assert!(src[r].contains("marrow"), "the span must cover the value");
    }

    #[test]
    fn corrupt_toml_is_reported_as_corrupt_not_as_a_crash() {
        let e = parse("bad.toml", "a = = 1\n").unwrap_err();
        assert_eq!(e.code(), Code::ParCorrupt);
        assert!(e.code().isolates_to_one_file());
    }

    #[test]
    fn json_members_get_spans_that_cover_key_and_value() {
        let src = "{\n  \"a\": 1,\n  \"b\": { \"c\": [10, 20] },\n  \"d\": \"x\"\n}\n";
        let a = parse("x.json", src).unwrap();
        a.validate().unwrap();
        let p = paths(&a);
        assert!(p.contains(&"a".to_string()));
        assert!(p.contains(&"b".to_string()));
        assert!(p.contains(&"b.c".to_string()));
        assert!(p.contains(&"d".to_string()));

        let b_node = a
            .nodes
            .iter()
            .find(|n| n.attrs.key_path.as_deref() == Some("b"))
            .unwrap();
        let r = b_node.byte_range().unwrap();
        assert_eq!(&src[r], "\"b\": { \"c\": [10, 20] }");
    }

    #[test]
    fn corrupt_json_is_reported_as_corrupt() {
        let e = parse("x.json", "{\"a\": }").unwrap_err();
        assert_eq!(e.code(), Code::ParCorrupt);
    }

    #[test]
    fn yaml_top_level_and_nested_keys_get_spans() {
        let src = "\
name: build
on:
  push:
    branches: [main]
jobs:
  test:
    runs-on: macos-latest
";
        let a = parse("ci.yaml", src).unwrap();
        a.validate().unwrap();
        let p = paths(&a);
        assert!(p.contains(&"name".to_string()), "got {p:?}");
        assert!(p.contains(&"on".to_string()));
        assert!(p.contains(&"on.push".to_string()));
        assert!(p.contains(&"jobs.test".to_string()));

        let name = a
            .nodes
            .iter()
            .find(|n| n.attrs.key_path.as_deref() == Some("name"))
            .unwrap();
        assert_eq!(name.text(), Some("name: build"));

        let on = a
            .nodes
            .iter()
            .find(|n| n.attrs.key_path.as_deref() == Some("on"))
            .unwrap();
        let r = on.byte_range().unwrap();
        assert!(
            src[r].contains("branches"),
            "a mapping's span covers its children"
        );
    }

    #[test]
    fn a_yaml_block_scalar_does_not_invent_keys() {
        let src = "script: |\n  echo hello: world\n  echo again: yes\nafter: 1\n";
        let a = parse("x.yml", src).unwrap();
        let p = paths(&a);
        assert_eq!(p, vec!["script".to_string(), "after".to_string()]);
    }

    #[test]
    fn corrupt_yaml_is_reported_as_corrupt() {
        let e = parse("x.yaml", "a:\n - b\n  - c\n\t- d\n").unwrap_err();
        assert_eq!(e.code(), Code::ParCorrupt);
    }

    #[test]
    fn the_key_colon_finder_ignores_quotes_and_comments() {
        assert_eq!(key_colon("a: 1"), Some(1));
        assert_eq!(key_colon("\"a: b\": 1"), Some(6));
        assert_eq!(key_colon("url: http://x"), Some(3));
        assert_eq!(key_colon("# a: 1"), None);
        assert_eq!(key_colon("- item"), None);
        assert_eq!(key_colon("plain text"), None);
    }

    #[test]
    fn nesting_stops_at_the_structured_depth_budget() {
        let src = "{\"a\":{\"b\":{\"c\":{\"d\":1}}}}";
        let probe = FileProbe::new("deep.json", src.len() as u64);
        let a = StructuredParser
            .parse(ParseInput {
                bytes: src.as_bytes(),
                probe: &probe,
                budget: BudgetGuard::new(Budgets {
                    max_structured_depth: 2,
                    ..Budgets::default()
                }),
            })
            .unwrap();
        let p = paths(&a);
        assert_eq!(p, vec!["a".to_string(), "a.b".to_string()]);
        a.validate().unwrap();
    }
}
