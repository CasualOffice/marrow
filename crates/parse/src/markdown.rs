//! Markdown (T1). 289 files in the real corpus — M0 §6 priority 3, and the
//! format the specification itself is written in.
//!
//! PAR-009: headings, sections and links keep their relationships. The section
//! tree is built by parent-chaining [`IrKind::Heading`] nodes — an `h2` names
//! the `h1` above it as its parent, and every block between them names the
//! `h2`. That is what makes CHK-002's "context prefix" (the heading path shown
//! above a citation) a walk up `parent` rather than a re-parse.
//!
//! # Why heading nodes chain rather than nesting inside `Section` nodes
//!
//! A `Section` node spanning heading-to-next-heading would be more faithful to
//! the document, and it is what Part 1 §8.6 gestures at. It also makes the
//! parent of an `h2` a *section*, not the `h1` — so the heading path, the one
//! thing every consumer wants, becomes a two-hop walk through nodes that carry
//! no text. Chaining the headings directly gives the same tree with half the
//! nodes and a `parent` link that means what a reader expects.

use std::ops::Range;

use marrow_core::{Code, Error, Result, SourceSpan};
use pulldown_cmark::{
    CodeBlockKind, Event, HeadingLevel, Options, Parser as MdParser, Tag, TagEnd,
};

use crate::decode;
use crate::ir::{
    ArtifactBuilder, IrKind, IrNode, LineIndex, NodeAttrs, ParseOutcome, ParseWarning,
    ParsedArtifact, ParserTier,
};
use crate::parser::{ContentParser, FileProbe, ParseInput};

/// The T1 Markdown parser.
#[derive(Clone, Copy, Debug, Default)]
pub struct MarkdownParser;

impl MarkdownParser {
    pub const ID: &'static str = "markdown";
    pub const VERSION: &'static str = "1";

    const EXTENSIONS: &'static [&'static str] = &["md", "markdown", "mdown", "mkd", "mkdn"];
}

impl ContentParser for MarkdownParser {
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
        probe.has_any_extension(Self::EXTENSIONS)
            || probe.mime_hint.as_deref() == Some("text/markdown")
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedArtifact> {
        let decoded = decode::decode(input.bytes)?;
        let src = decoded.text.as_str();
        if src.trim().is_empty() {
            return Err(Error::new(
                Code::ParLowYield,
                "This Markdown file is empty, so only its metadata is indexed.",
            ));
        }

        let mut b = ArtifactBuilder::new(Self::ID, Self::VERSION, self.tier(), input.budget);
        b.degrade_provenance(decoded.provenance_ceiling());

        Walker::new(src).run(&mut b)?;

        if b.node_count() == 0 {
            return Err(Error::new(
                Code::ParLowYield,
                "This Markdown file contains no headings, prose or code, so only its metadata \
                 is indexed.",
            ));
        }
        if decoded.is_low_yield() {
            b.warn(ParseWarning::new(
                Code::ParLowYield,
                "This Markdown file did not decode cleanly as text; the structure was still \
                 extracted. Re-save it as UTF-8 for exact provenance.",
            ));
            b.set_outcome(ParseOutcome::LowYield);
        }
        Ok(b.finish())
    }
}

/// A block whose text is only known once its `End` event arrives.
#[derive(Debug)]
struct Open {
    kind: IrKind,
    /// Full source range of the element, from the `Start` event.
    range: Range<usize>,
    text: String,
    attrs: NodeAttrs,
    /// Links closed inside this block, emitted as its children once it exists.
    links: Vec<Open>,
}

struct Walker<'a> {
    src: &'a str,
    lines: LineIndex,
    /// (level, arena index) of the heading chain currently open.
    headings: Vec<(u8, usize)>,
    /// Blocks whose text is still being collected, innermost last.
    open: Vec<Open>,
    /// Arena index of the enclosing table, and of the current row.
    table: Option<usize>,
    row: Option<usize>,
    row_no: u32,
    col_no: u32,
    /// Header cell texts, so body cells can name their column (TBL-003 in
    /// miniature: a cell that knows its column heading is worth far more to
    /// retrieval than one that only knows it is the third field).
    header: Vec<String>,
    in_header: bool,
}

impl<'a> Walker<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            lines: LineIndex::new(src),
            headings: Vec::new(),
            open: Vec::new(),
            table: None,
            row: None,
            row_no: 0,
            col_no: 0,
            header: Vec::new(),
            in_header: false,
        }
    }

    fn run(&mut self, b: &mut ArtifactBuilder) -> Result<()> {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TASKLISTS);
        options.insert(Options::ENABLE_FOOTNOTES);
        // Front matter. Both spellings, because both are in the wild.
        options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
        options.insert(Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS);

        for (event, range) in MdParser::new_ext(self.src, options).into_offset_iter() {
            b.budget().check_time()?;
            match event {
                Event::Start(tag) => self.start(tag, range, b)?,
                Event::End(tag) => self.end(tag, b)?,
                Event::Text(t) | Event::Code(t) => self.push_text(&t),
                Event::InlineMath(t) | Event::DisplayMath(t) => self.push_text(&t),
                Event::SoftBreak | Event::HardBreak => self.push_text("\n"),
                Event::Html(html) | Event::InlineHtml(html)
                    if html.trim_start().starts_with("<!--") =>
                {
                    let parent = self.current_parent();
                    let r = trim_range(self.src, range);
                    let node = IrNode::verbatim(IrKind::Comment, self.src, r.clone())?
                        .with_attrs(NodeAttrs::default().with_lines(&self.lines, &r));
                    b.push(parent, node)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn start(&mut self, tag: Tag<'_>, range: Range<usize>, b: &mut ArtifactBuilder) -> Result<()> {
        match tag {
            Tag::Heading { level, .. } => self.open_block(
                IrKind::Heading,
                range,
                NodeAttrs {
                    level: Some(level_of(level)),
                    ..Default::default()
                },
            ),
            Tag::Paragraph => self.open_block(IrKind::Paragraph, range, NodeAttrs::default()),
            Tag::Item => self.open_block(IrKind::ListItem, range, NodeAttrs::default()),
            Tag::CodeBlock(kind) => {
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                    CodeBlockKind::Indented => None,
                };
                self.open_block(
                    IrKind::CodeBlock,
                    range,
                    NodeAttrs {
                        language,
                        ..Default::default()
                    },
                );
            }
            Tag::MetadataBlock(_) => {
                self.open_block(IrKind::FrontMatter, range, NodeAttrs::default())
            }
            Tag::Link { dest_url, .. } => self.open_block(
                IrKind::Link,
                range,
                NodeAttrs {
                    // Lifted from the file, so untrusted like any other file
                    // text. Nothing downstream may follow it automatically.
                    url: Some(dest_url.to_string()),
                    ..Default::default()
                },
            ),
            Tag::Table(_) => {
                let node = IrNode::structural(IrKind::Table, bytes(&range))
                    .with_attrs(NodeAttrs::default().with_lines(&self.lines, &range));
                let parent = self.current_parent();
                self.table = Some(b.push(parent, node)?);
                self.row_no = 0;
                self.header.clear();
            }
            Tag::TableHead | Tag::TableRow => {
                self.in_header = matches!(tag, Tag::TableHead);
                let node =
                    IrNode::structural(IrKind::TableRow, bytes(&range)).with_attrs(NodeAttrs {
                        row: Some(self.row_no),
                        ..NodeAttrs::default().with_lines(&self.lines, &range)
                    });
                self.row = Some(b.push(self.table, node)?);
                self.col_no = 0;
            }
            Tag::TableCell => {
                let column_name = self.header.get(self.col_no as usize).cloned();
                self.open_block(
                    IrKind::TableCell,
                    range,
                    NodeAttrs {
                        row: Some(self.row_no),
                        col: Some(self.col_no),
                        column_name,
                        ..Default::default()
                    },
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self, tag: TagEnd, b: &mut ArtifactBuilder) -> Result<()> {
        match tag {
            TagEnd::Heading(_)
            | TagEnd::Paragraph
            | TagEnd::Item
            | TagEnd::CodeBlock
            | TagEnd::MetadataBlock(_) => self.close_block(b),
            TagEnd::TableCell => {
                if self.in_header {
                    let text = self.open.last().map(|o| o.text.trim().to_owned());
                    self.header.push(text.unwrap_or_default());
                }
                self.col_no += 1;
                self.close_block(b)
            }
            TagEnd::Link => {
                // A link's text belongs to its enclosing block as well, so the
                // paragraph reads as written. The link itself becomes a child
                // node once the block is pushed.
                let Some(link) = self.open.pop() else {
                    return Ok(());
                };
                if let Some(o) = self.open.last_mut() {
                    o.text.push_str(&link.text);
                    o.links.push(link);
                }
                Ok(())
            }
            TagEnd::Table => {
                self.table = None;
                self.row = None;
                Ok(())
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                self.in_header = false;
                self.row = None;
                self.row_no += 1;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn open_block(&mut self, kind: IrKind, range: Range<usize>, attrs: NodeAttrs) {
        self.open.push(Open {
            kind,
            range,
            text: String::new(),
            attrs,
            links: Vec::new(),
        });
    }

    fn push_text(&mut self, t: &str) {
        if let Some(o) = self.open.last_mut() {
            o.text.push_str(t);
        }
    }

    /// Close the innermost open block and emit it, unless it is a paragraph
    /// directly inside a list item — a loose list would otherwise produce a
    /// `ListItem` and a `Paragraph` holding the same words twice.
    fn close_block(&mut self, b: &mut ArtifactBuilder) -> Result<()> {
        let Some(done) = self.open.pop() else {
            return Ok(());
        };

        if done.kind == IrKind::Paragraph
            && self.open.last().map(|o| o.kind) == Some(IrKind::ListItem)
        {
            if let Some(item) = self.open.last_mut() {
                if !item.text.is_empty() {
                    item.text.push('\n');
                }
                item.text.push_str(&done.text);
                item.links.extend(done.links);
            }
            return Ok(());
        }

        if done.text.trim().is_empty() && done.kind != IrKind::CodeBlock {
            return Ok(());
        }

        let idx = self.emit(done, b)?;
        let _ = idx;
        Ok(())
    }

    /// Push one collected block plus its links. Returns the block's index.
    fn emit(&mut self, done: Open, b: &mut ArtifactBuilder) -> Result<usize> {
        let range = trim_range(self.src, done.range.clone());
        let (text, clipped) = b.budget().clamp_text(done.text.trim());
        let attrs = done.attrs.clone().with_lines(&self.lines, &range);
        let level = attrs.level;

        let parent = if done.kind == IrKind::Heading {
            let level = level.unwrap_or(1);
            while self.headings.last().is_some_and(|(l, _)| *l >= level) {
                self.headings.pop();
            }
            self.headings.last().map(|(_, i)| *i)
        } else if done.kind == IrKind::TableCell {
            self.row.or(self.table)
        } else {
            self.current_parent()
        };

        let node = IrNode::content_in(done.kind, self.src, range, text)?.with_attrs(attrs);
        let idx = b.push(parent, node)?;

        if clipped {
            b.warn(ParseWarning::new(
                Code::ParTruncated,
                "A block in this file was larger than the per-node text budget and its text \
                 was clipped. The byte span still covers the whole block.",
            ));
            b.set_outcome(ParseOutcome::Partial);
        }

        if done.kind == IrKind::Heading {
            self.headings.push((level.unwrap_or(1), idx));
        }

        for link in done.links {
            let lrange = trim_range(self.src, link.range);
            let node = IrNode::content_in(IrKind::Link, self.src, lrange.clone(), link.text)?
                .with_attrs(NodeAttrs {
                    url: link.attrs.url,
                    ..NodeAttrs::default().with_lines(&self.lines, &lrange)
                });
            b.push(Some(idx), node)?;
        }
        Ok(idx)
    }

    /// Innermost structural container, else the open heading, else the root.
    fn current_parent(&self) -> Option<usize> {
        self.row
            .or(self.table)
            .or_else(|| self.headings.last().map(|(_, i)| *i))
    }
}

fn level_of(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn bytes(range: &Range<usize>) -> SourceSpan {
    SourceSpan::Bytes {
        start: range.start as u64,
        end: range.end as u64,
    }
}

/// Shrink a range to its non-whitespace content. Keeps the span honest: a
/// paragraph's span should not include the blank line that ended it.
fn trim_range(src: &str, range: Range<usize>) -> Range<usize> {
    let Some(s) = src.get(range.clone()) else {
        return range;
    };
    let start = range.start + (s.len() - s.trim_start().len());
    let end = range.start + s.trim_end().len();
    start.min(end)..end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetGuard, Budgets};

    fn parse(src: &str) -> ParsedArtifact {
        let probe = FileProbe::new("doc.md", src.len() as u64);
        MarkdownParser
            .parse(ParseInput {
                bytes: src.as_bytes(),
                probe: &probe,
                budget: BudgetGuard::new(Budgets::default()),
            })
            .expect("fixture must parse")
    }

    #[test]
    fn markdown_parser_extracts_headings_prose_code_and_links() {
        let src = "\
---
title: Notes
---

# Top

Some prose with a [link](https://example.invalid/x).

## Under

```rust
fn main() {}
```

- one
- two
";
        let a = parse(src);
        a.validate().unwrap();

        let kinds: Vec<_> = a.nodes.iter().map(|n| n.kind).collect();
        assert!(kinds.contains(&IrKind::FrontMatter));
        assert!(kinds.contains(&IrKind::Heading));
        assert!(kinds.contains(&IrKind::Paragraph));
        assert!(kinds.contains(&IrKind::CodeBlock));
        assert!(kinds.contains(&IrKind::Link));
        assert_eq!(kinds.iter().filter(|k| **k == IrKind::ListItem).count(), 2);

        let code = a
            .nodes
            .iter()
            .find(|n| n.kind == IrKind::CodeBlock)
            .expect("code block");
        assert_eq!(code.attrs.language.as_deref(), Some("rust"));
        assert_eq!(code.text().map(str::trim), Some("fn main() {}"));

        let link = a.nodes.iter().find(|n| n.kind == IrKind::Link).unwrap();
        assert_eq!(link.attrs.url.as_deref(), Some("https://example.invalid/x"));
        assert_eq!(link.text(), Some("link"));
    }

    #[test]
    fn heading_parents_pop_back_to_the_right_level() {
        let a = parse("# One\n\ntext\n\n## Two\n\n### Three\n\n## Four\n");
        let headings: Vec<_> = a
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.kind == IrKind::Heading)
            .collect();
        assert_eq!(headings.len(), 4);

        let (h1_idx, h1) = headings[0];
        let (h2_idx, h2) = headings[1];
        let (_, h3) = headings[2];
        let (_, h4) = headings[3];

        assert_eq!(h1.parent, None, "the top heading has no parent");
        assert_eq!(h2.parent, Some(h1_idx), "h2 under h1 has the h1 as parent");
        assert_eq!(h3.parent, Some(h2_idx), "h3 nests under the h2");
        assert_eq!(h4.parent, Some(h1_idx), "a sibling h2 pops back to the h1");

        // Body content hangs off the heading it sits under.
        let para = a
            .nodes
            .iter()
            .find(|n| n.kind == IrKind::Paragraph)
            .unwrap();
        assert_eq!(para.parent, Some(h1_idx));
    }

    #[test]
    fn a_table_yields_rows_and_cells_that_know_their_column() {
        let a = parse("| name | qty |\n|---|---|\n| bolt | 12 |\n");
        assert_eq!(
            a.nodes.iter().filter(|n| n.kind == IrKind::Table).count(),
            1
        );
        assert_eq!(
            a.nodes
                .iter()
                .filter(|n| n.kind == IrKind::TableRow)
                .count(),
            2,
            "header row plus one body row"
        );
        let cells: Vec<_> = a
            .nodes
            .iter()
            .filter(|n| n.kind == IrKind::TableCell)
            .collect();
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[0].text(), Some("name"));
        assert_eq!(cells[3].text(), Some("12"));
        assert_eq!(cells[3].attrs.column_name.as_deref(), Some("qty"));
        assert_eq!(cells[3].attrs.row, Some(1));
        assert_eq!(cells[3].attrs.col, Some(1));
        a.validate().unwrap();
    }

    #[test]
    fn an_html_comment_is_kept_as_a_comment_node() {
        let a = parse("# T\n\n<!-- a note -->\n\nbody\n");
        let c = a.nodes.iter().find(|n| n.kind == IrKind::Comment).unwrap();
        assert!(c.text().unwrap_or_default().contains("a note"));
    }

    #[test]
    fn front_matter_is_untrusted_content_like_everything_else_in_the_file() {
        let a = parse("---\nrole: system\n---\n\n# x\n");
        let fm = a
            .nodes
            .iter()
            .find(|n| n.kind == IrKind::FrontMatter)
            .unwrap();
        assert_eq!(fm.trust(), crate::ir::Trust::UntrustedContent);
    }
}
