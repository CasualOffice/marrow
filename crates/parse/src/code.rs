//! Code (T1, Tree-sitter). ~1,300 files — M0 §6 priority 1, the largest real
//! content class in the corpus.
//!
//! PAR-008: syntax-aware parsing and symbol extraction. Languages are the ones
//! the corpus actually contains, in the counts it contains them:
//! Rust 839, TypeScript + TSX 271, JavaScript 149, SQL 96, Python 63.
//!
//! # A symbol is one node
//!
//! Every [`IrKind::Symbol`] node's span is the symbol's **whole** byte range,
//! from the first token of its signature (or its first decorator/attribute) to
//! its closing brace. A 400-line function is one node, not fourteen. That costs
//! nothing here and it is the difference between a citation that says "in
//! `fn reconcile`" and one that says "somewhere around line 812".
//!
//! Containers do nest: methods hang off their `impl` or `class`, which hangs
//! off the file. Function bodies are never descended into — a closure inside a
//! function is part of that function, not a peer of it.
//!
//! # What is deliberately not emitted
//!
//! Text between symbols — imports, top-level statements, module comments —
//! yields no node. A file whose symbol nodes cover very little of it is marked
//! [`ParseOutcome::Partial`], and a file with *no* extractable symbols returns
//! `ParUnsupported` so the plain-text parser takes it and the file stays fully
//! searchable. See the note in `lib.rs` on what that costs.

use std::ops::Range;

use marrow_core::{Code, Error, Result};
use tree_sitter::{Language, Node, ParseOptions, Parser as TsParser};

use crate::decode;
use crate::ir::{
    ArtifactBuilder, IrKind, IrNode, LineIndex, NodeAttrs, ParseOutcome, ParseWarning,
    ParsedArtifact, ParserTier, SymbolKind,
};
use crate::parser::{ContentParser, FileProbe, ParseInput};

/// Below this fraction of non-whitespace bytes covered by symbols, the parse is
/// reported as `Partial`: something real is in the file that we did not model.
const MIN_COVERAGE_FOR_OK: f32 = 0.5;

/// A source language we have a grammar for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Sql,
}

impl Lang {
    /// Route on the file name alone — `handles()` never sees bytes.
    pub fn from_probe(probe: &FileProbe) -> Option<Self> {
        let ext = probe.extension.as_deref()?;
        Some(match ext {
            "rs" => Lang::Rust,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
            "py" | "pyi" => Lang::Python,
            "sql" => Lang::Sql,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "javascript",
            Lang::Python => "python",
            Lang::Sql => "sql",
        }
    }

    fn grammar(self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::Sql => tree_sitter_sequel::LANGUAGE.into(),
        }
    }
}

/// Node kinds that are pure structure: recurse through them, emit nothing.
///
/// Everything not listed here and not a symbol is ignored, which is what keeps
/// a 5,000-node syntax tree from becoming 5,000 IR nodes.
const TRANSPARENT: &[&str] = &[
    // roots
    "source_file",
    "program",
    "module",
    // Rust
    "declaration_list",
    // TS/JS
    "export_statement",
    "ambient_declaration",
    "internal_module",
    "class_body",
    // Python
    "block",
    // SQL (tree-sitter-sequel wraps every statement)
    "statement",
];

/// `(node kind, symbol kind, descend into it)` per language.
///
/// "Descend" is true only for containers whose children are themselves worth
/// naming. It is false for functions: a nested `fn` is part of its parent's
/// body, and splitting it out would break the one-symbol-one-node rule.
fn symbol_table(lang: Lang) -> &'static [(&'static str, SymbolKind, bool)] {
    match lang {
        Lang::Rust => &[
            ("function_item", SymbolKind::Function, false),
            ("function_signature_item", SymbolKind::Function, false),
            ("struct_item", SymbolKind::Struct, false),
            ("union_item", SymbolKind::Struct, false),
            ("enum_item", SymbolKind::Enum, false),
            ("trait_item", SymbolKind::Trait, true),
            ("impl_item", SymbolKind::Impl, true),
            ("mod_item", SymbolKind::Module, true),
            ("type_item", SymbolKind::TypeAlias, false),
            ("const_item", SymbolKind::Constant, false),
            ("static_item", SymbolKind::Constant, false),
            ("macro_definition", SymbolKind::Function, false),
        ],
        Lang::TypeScript | Lang::Tsx => &[
            ("function_declaration", SymbolKind::Function, false),
            (
                "generator_function_declaration",
                SymbolKind::Function,
                false,
            ),
            ("class_declaration", SymbolKind::Class, true),
            ("abstract_class_declaration", SymbolKind::Class, true),
            ("interface_declaration", SymbolKind::Interface, false),
            ("type_alias_declaration", SymbolKind::TypeAlias, false),
            ("enum_declaration", SymbolKind::Enum, false),
            ("method_definition", SymbolKind::Method, false),
            ("abstract_method_signature", SymbolKind::Method, false),
            ("lexical_declaration", SymbolKind::Constant, false),
            ("variable_declaration", SymbolKind::Constant, false),
        ],
        Lang::JavaScript => &[
            ("function_declaration", SymbolKind::Function, false),
            (
                "generator_function_declaration",
                SymbolKind::Function,
                false,
            ),
            ("class_declaration", SymbolKind::Class, true),
            ("method_definition", SymbolKind::Method, false),
            ("lexical_declaration", SymbolKind::Constant, false),
            ("variable_declaration", SymbolKind::Constant, false),
        ],
        Lang::Python => &[
            ("function_definition", SymbolKind::Function, false),
            ("class_definition", SymbolKind::Class, true),
            ("decorated_definition", SymbolKind::Function, true),
        ],
        Lang::Sql => &[
            ("create_table", SymbolKind::Table, false),
            ("create_view", SymbolKind::View, false),
            ("create_materialized_view", SymbolKind::View, false),
            ("create_index", SymbolKind::Index, false),
            ("create_function", SymbolKind::Routine, false),
            ("create_procedure", SymbolKind::Routine, false),
            ("create_trigger", SymbolKind::Routine, false),
            ("create_type", SymbolKind::TypeAlias, false),
            ("create_schema", SymbolKind::Module, false),
        ],
    }
}

/// The T1 code parser.
#[derive(Clone, Copy, Debug, Default)]
pub struct CodeParser;

impl CodeParser {
    pub const ID: &'static str = "code";
    /// Bump when a grammar version or the symbol table changes: PAR-003 uses
    /// this to schedule reprocessing, and a grammar upgrade genuinely does
    /// change the output.
    pub const VERSION: &'static str = "1";

    pub fn new() -> Self {
        Self
    }
}

impl ContentParser for CodeParser {
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
        Lang::from_probe(probe).is_some()
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedArtifact> {
        let lang = Lang::from_probe(input.probe).ok_or_else(|| {
            Error::new(
                Code::ParUnsupported,
                "No Tree-sitter grammar is bundled for this file's language.",
            )
        })?;

        let decoded = decode::decode(input.bytes)?;
        let src = decoded.text.as_str();
        if src.trim().is_empty() {
            return Err(Error::new(
                Code::ParLowYield,
                "This source file is empty, so only its metadata is indexed.",
            ));
        }

        let mut b = ArtifactBuilder::new(Self::ID, Self::VERSION, self.tier(), input.budget);
        b.degrade_provenance(decoded.provenance_ceiling());

        let mut ts = TsParser::new();
        ts.set_language(&lang.grammar()).map_err(|e| {
            // A grammar/ABI mismatch is a build problem, not a file problem, so
            // it must not be reported as a corrupt file.
            Error::invariant(
                "A bundled Tree-sitter grammar does not match the linked library version. \
                 Rebuild the crate; this is not a problem with the file.",
            )
            .with_source(e)
        })?;

        let bytes_ref = src.as_bytes();
        let budget = *b.budget();
        // PAR-010/011: Tree-sitter is C code parsing hostile input. The progress
        // callback is the only place a runaway parse can be stopped from safe
        // Rust, and it returns `true` to cancel.
        let mut cancel = |_state: &tree_sitter::ParseState| budget.check_time().is_err();
        let mut read = |offset: usize, _pos: tree_sitter::Point| -> &[u8] {
            bytes_ref.get(offset..).unwrap_or_default()
        };
        let tree = ts
            .parse_with_options(
                &mut read,
                None,
                Some(ParseOptions::new().progress_callback(&mut cancel)),
            )
            .ok_or_else(|| {
                // The only way `None` comes back here is the cancellation we
                // asked for, so report it as the budget it was.
                Error::new(
                    Code::ParBudgetExceeded,
                    "Parsing this source file exceeded the per-file time budget, so it is \
                     indexed by metadata only.",
                )
            })?;

        let mut w = Walk {
            src,
            lines: LineIndex::new(src),
            lang,
            symbols: symbol_table(lang),
            covered: 0,
        };
        w.children(tree.root_node(), None, &mut b)?;

        if b.node_count() == 0 {
            // Not a failure: a file of imports and statements has no symbols.
            // Declining lets the plain-text parser index it in full.
            return Err(Error::new(
                Code::ParUnsupported,
                "No top-level code symbols were found in this file; it is indexed as plain \
                 text instead.",
            ));
        }

        if tree.root_node().has_error() {
            b.warn(ParseWarning::new(
                Code::ParTruncated,
                "This source file did not parse cleanly. The symbols that were recognised are \
                 indexed; check the file for a syntax error if you expected more.",
            ));
            b.set_outcome(ParseOutcome::Partial);
        }

        let significant = src.chars().filter(|c| !c.is_whitespace()).count().max(1);
        let coverage = w.covered as f32 / significant as f32;
        if coverage < MIN_COVERAGE_FOR_OK {
            b.set_outcome(ParseOutcome::Partial);
        }

        Ok(b.finish())
    }
}

struct Walk<'a> {
    src: &'a str,
    lines: LineIndex,
    lang: Lang,
    symbols: &'static [(&'static str, SymbolKind, bool)],
    /// Non-whitespace bytes covered by top-level symbols.
    covered: usize,
}

impl Walk<'_> {
    /// Visit `node`'s named children, emitting symbols and recursing through
    /// structure.
    fn children(
        &mut self,
        node: Node<'_>,
        parent: Option<usize>,
        b: &mut ArtifactBuilder,
    ) -> Result<()> {
        b.budget().check_time()?;
        let mut cursor = node.walk();
        let kids: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        drop(cursor);
        for child in kids {
            self.visit(child, parent, b)?;
        }
        Ok(())
    }

    fn visit(
        &mut self,
        node: Node<'_>,
        parent: Option<usize>,
        b: &mut ArtifactBuilder,
    ) -> Result<()> {
        let kind = node.kind();

        if TRANSPARENT.contains(&kind) {
            return self.children(node, parent, b);
        }

        let Some((_, symbol_kind, descend)) =
            self.symbols.iter().copied().find(|(k, _, _)| *k == kind)
        else {
            return Ok(());
        };

        // Python decorators: the symbol is the decorated definition, so the
        // span keeps the decorators, but the name and kind come from inside.
        let (inner, symbol_kind) = if kind == "decorated_definition" {
            match node.child_by_field_name("definition") {
                Some(d) => {
                    let k = if d.kind() == "class_definition" {
                        SymbolKind::Class
                    } else {
                        SymbolKind::Function
                    };
                    (d, k)
                }
                None => return Ok(()),
            }
        } else {
            (node, symbol_kind)
        };

        // `const x = () => {}` is how most TS/JS code declares a function. A
        // lexical declaration that binds anything else is not a symbol.
        if matches!(kind, "lexical_declaration" | "variable_declaration")
            && !declares_a_callable(inner)
        {
            return Ok(());
        }

        let range: Range<usize> = node.byte_range();
        let Some(name) = self.name_of(inner) else {
            return Ok(());
        };

        let (text, clipped) = b
            .budget()
            .clamp_text(self.src.get(range.clone()).unwrap_or(""));
        let attrs = NodeAttrs {
            symbol_kind: Some(symbol_kind),
            name: Some(name),
            language: Some(self.lang.as_str().to_owned()),
            ..NodeAttrs::default().with_lines(&self.lines, &range)
        };
        let idx = b.push(
            parent,
            IrNode::content_in(IrKind::Symbol, self.src, range.clone(), text)?.with_attrs(attrs),
        )?;

        if clipped {
            b.warn(ParseWarning::new(
                Code::ParTruncated,
                "A very large symbol's text was clipped to the per-node budget. Its byte span \
                 still covers the whole symbol.",
            ));
            b.set_outcome(ParseOutcome::Partial);
        }

        if parent.is_none() {
            self.covered += self
                .src
                .get(range)
                .map(|s| s.chars().filter(|c| !c.is_whitespace()).count())
                .unwrap_or(0);
        }

        if descend {
            // Recurse into the container, not into a function body: `inner` is
            // the declaration itself, whose transparent children (a
            // `declaration_list`, a `class_body`, a Python `block`) hold the
            // members worth naming.
            self.children(inner, Some(idx), b)?;
        }
        Ok(())
    }

    /// The declared name, or `None` when the construct is anonymous.
    fn name_of(&self, node: Node<'_>) -> Option<String> {
        if let Some(n) = node.child_by_field_name("name") {
            return self.text_of(n);
        }
        // Rust `impl Foo` and `impl Trait for Foo` have no `name` field.
        if node.kind() == "impl_item" {
            let ty = self.text_of(node.child_by_field_name("type")?)?;
            return Some(
                match node
                    .child_by_field_name("trait")
                    .and_then(|t| self.text_of(t))
                {
                    Some(tr) => format!("{tr} for {ty}"),
                    None => ty,
                },
            );
        }
        // TS/JS `const x = () => {}`: the name is on the declarator.
        if matches!(node.kind(), "lexical_declaration" | "variable_declaration") {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "variable_declarator")
                .and_then(|d| d.child_by_field_name("name"))
                .and_then(|n| self.text_of(n));
            return found;
        }
        // SQL names hang off an `object_reference`, or are a bare identifier.
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "object_reference" => {
                    return child
                        .child_by_field_name("name")
                        .and_then(|n| self.text_of(n))
                        .or_else(|| self.text_of(child));
                }
                "identifier" | "type_identifier" | "field_identifier" => {
                    return self.text_of(child);
                }
                _ => {}
            }
        }
        None
    }

    fn text_of(&self, node: Node<'_>) -> Option<String> {
        self.src.get(node.byte_range()).map(str::to_owned)
    }
}

/// Whether a `const`/`let`/`var` declaration binds a function or class.
fn declares_a_callable(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "variable_declarator")
        .filter_map(|d| d.child_by_field_name("value"))
        .any(|v| {
            matches!(
                v.kind(),
                "arrow_function"
                    | "function"
                    | "function_expression"
                    | "class"
                    | "generator_function"
            )
        });
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetGuard, Budgets};

    fn parse(name: &str, src: &str) -> Result<ParsedArtifact> {
        let probe = FileProbe::new(name, src.len() as u64);
        CodeParser::new().parse(ParseInput {
            bytes: src.as_bytes(),
            probe: &probe,
            budget: BudgetGuard::new(Budgets::default()),
        })
    }

    fn names(a: &ParsedArtifact) -> Vec<String> {
        a.nodes
            .iter()
            .filter_map(|n| n.attrs.name.clone())
            .collect()
    }

    #[test]
    fn code_parser_extracts_rust_symbols_with_their_ranges() {
        let src = "\
use std::io;

/// Doc comment.
pub struct Widget {
    id: u32,
}

impl Widget {
    pub fn id(&self) -> u32 {
        self.id
    }
}

pub fn build() -> Widget {
    Widget { id: 1 }
}
";
        let a = parse("widget.rs", src).unwrap();
        a.validate().unwrap();
        assert_eq!(names(&a), vec!["Widget", "Widget", "id", "build"]);

        let impl_node = a
            .nodes
            .iter()
            .find(|n| n.attrs.symbol_kind == Some(SymbolKind::Impl))
            .unwrap();
        let method = a
            .nodes
            .iter()
            .find(|n| {
                n.attrs.symbol_kind == Some(SymbolKind::Method)
                    || n.attrs.name.as_deref() == Some("id")
                        && n.attrs.symbol_kind == Some(SymbolKind::Function)
            })
            .unwrap();
        assert_eq!(
            method.parent,
            Some(impl_node.ordinal as usize),
            "a method hangs off its impl block"
        );
        assert_eq!(
            a.nodes[0].attrs.language.as_deref(),
            Some("rust"),
            "the language is runtime-derived structure"
        );
    }

    #[test]
    fn a_long_function_stays_a_single_node() {
        let body: String = (0..400).map(|i| format!("    let v{i} = {i};\n")).collect();
        let src = format!("fn long() {{\n{body}}}\n");
        let a = parse("long.rs", &src).unwrap();
        let symbols: Vec<_> = a
            .nodes
            .iter()
            .filter(|n| n.kind == IrKind::Symbol)
            .collect();
        assert_eq!(symbols.len(), 1, "one function is one node");
        let r = symbols[0].byte_range().unwrap();
        assert_eq!(r.start, 0);
        assert_eq!(r.end, src.trim_end().len());
    }

    #[test]
    fn typescript_and_tsx_symbols_include_arrow_consts() {
        let ts = "export interface Props { a: number }\nexport class Box {\n  render() {}\n}\nexport const useThing = () => 1;\nconst plain = 4;\n";
        let a = parse("thing.ts", ts).unwrap();
        let got = names(&a);
        assert!(got.contains(&"Props".to_string()));
        assert!(got.contains(&"Box".to_string()));
        assert!(got.contains(&"render".to_string()));
        assert!(got.contains(&"useThing".to_string()));
        assert!(
            !got.contains(&"plain".to_string()),
            "a plain const is not a symbol"
        );

        let tsx = "export const App = () => <div />;\n";
        let a = parse("App.tsx", tsx).unwrap();
        assert_eq!(names(&a), vec!["App"]);
    }

    #[test]
    fn javascript_symbols_are_found() {
        let a = parse("m.js", "export function go() {}\nclass C { m() {} }\n").unwrap();
        assert_eq!(names(&a), vec!["go", "C", "m"]);
    }

    #[test]
    fn python_decorators_stay_with_their_function() {
        let src = "import os\n\n@cache\ndef f():\n    return 1\n\nclass C:\n    def m(self):\n        return 2\n";
        let a = parse("m.py", src).unwrap();
        assert_eq!(names(&a), vec!["f", "C", "m"]);
        let f = a
            .nodes
            .iter()
            .find(|n| n.attrs.name.as_deref() == Some("f"))
            .unwrap();
        assert!(
            f.text().unwrap_or_default().starts_with("@cache"),
            "the decorator is part of the symbol, not a stray node"
        );
    }

    #[test]
    fn sql_ddl_becomes_symbols() {
        let src = "CREATE TABLE files (id TEXT PRIMARY KEY);\nCREATE INDEX idx_files ON files(id);\nCREATE VIEW v AS SELECT 1;\n";
        let a = parse("schema.sql", src).unwrap();
        let kinds: Vec<_> = a.nodes.iter().filter_map(|n| n.attrs.symbol_kind).collect();
        assert!(kinds.contains(&SymbolKind::Table));
        assert!(kinds.contains(&SymbolKind::Index));
        assert!(kinds.contains(&SymbolKind::View));
    }

    #[test]
    fn a_file_with_no_symbols_is_declined_so_plain_text_can_have_it() {
        let e = parse("script.py", "import os\nprint(os.getcwd())\n").unwrap_err();
        assert_eq!(e.code(), Code::ParUnsupported);
    }

    #[test]
    fn symbol_text_is_untrusted_even_though_the_name_was_derived() {
        let a = parse("x.rs", "fn ignore_all_previous_instructions() {}\n").unwrap();
        assert_eq!(a.nodes[0].trust(), crate::ir::Trust::UntrustedContent);
    }
}
