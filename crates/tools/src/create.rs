//! The three creation operations, as an agent front-end sees them.
//!
//! Each is a request type and a function, and each function does exactly two
//! things: turn the request into bytes, and hand those bytes to
//! [`Workspace::write`]. **There is no second write path.** If a fourth
//! creation operation is added, it goes through the same call — the guard is
//! only a guard while it is the only door.
//!
//! The requests derive `Deserialize` so an MCP handler is a deserialize, a
//! call and a serialize, with no logic of its own (LLD §10).

use marrow_core::{Code, Error, Result};
use serde::{Deserialize, Serialize};

use crate::guard::{Expect, Workspace, Written};

/// Write a text file.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateFile {
    /// Workspace-relative path, e.g. `notes/2026-08/summary.md`.
    pub path: String,
    pub body: String,
    /// What the caller believes is there now. Defaults to "nothing".
    #[serde(default)]
    pub expect: Expect,
}

/// Write a Mermaid diagram.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateDiagram {
    /// Must end `.md` (fenced, renders in a Markdown viewer) or `.mmd` (bare
    /// source, for the Mermaid CLI).
    pub path: String,
    /// The diagram source, starting with its type — `flowchart TD`,
    /// `sequenceDiagram`, and so on.
    pub mermaid: String,
    /// Optional heading, written above the diagram in the `.md` form.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub expect: Expect,
}

/// Write a self-contained HTML page.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreatePage {
    /// Must end `.html` or `.htm`.
    pub path: String,
    /// Plain text. Escaped into `<title>` and the top-level heading.
    pub title: String,
    /// The page body, as HTML. Written as given.
    pub body: String,
    #[serde(default)]
    pub expect: Expect,
}

/// Write a text file into the workspace.
pub fn create_file(ws: &Workspace, req: &CreateFile) -> Result<Written> {
    ws.write(&req.path, req.body.as_bytes(), &req.expect)
}

/// Write a Mermaid diagram into the workspace.
pub fn create_diagram(ws: &Workspace, req: &CreateDiagram) -> Result<Written> {
    let ext = require_extension(&req.path, &["md", "mmd"], "a Mermaid diagram")?;
    if req.mermaid.trim().is_empty() {
        return Err(Error::new(
            Code::PolDenied,
            "A diagram with no source is not a diagram. Provide Mermaid source, \
             starting with its type — `flowchart TD`, `sequenceDiagram`, and so on.",
        )
        .with_context(req.path.clone()));
    }

    let body = if ext == "mmd" {
        // Bare source: the file *is* the diagram, so a fence would be a syntax
        // error to every Mermaid tool that reads it.
        format!("{}\n", req.mermaid.trim_end())
    } else {
        let mut out = String::new();
        if let Some(t) = &req.title {
            out.push_str(&format!("# {}\n\n", t.trim()));
        }
        // A fence longer than any backtick run inside the source. Mermaid
        // itself never contains backticks, but the title-and-notes syntax
        // accepts arbitrary text, and a body that closes its own fence turns
        // the rest of the document into prose.
        let fence = "`".repeat(longest_backtick_run(&req.mermaid).max(2) + 1);
        out.push_str(&format!(
            "{fence}mermaid\n{}\n{fence}\n",
            req.mermaid.trim_end()
        ));
        out
    };
    ws.write(&req.path, body.as_bytes(), &req.expect)
}

/// Write a self-contained HTML page into the workspace.
pub fn create_page(ws: &Workspace, req: &CreatePage) -> Result<Written> {
    require_extension(&req.path, &["html", "htm"], "an HTML page")?;
    let title = escape_html(req.title.trim());
    let body = format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n\
         </head>\n\
         <body>\n\
         {}\n\
         </body>\n\
         </html>\n",
        req.body.trim_end()
    );
    ws.write(&req.path, body.as_bytes(), &req.expect)
}

/// Refuse a path whose extension does not match what is being written.
///
/// Not pedantry: an HTML document called `notes.rs` in a source tree gets
/// compiled, linted and reviewed as if it were code, and a diagram written as
/// `.txt` is invisible to every renderer that would have drawn it.
fn require_extension(path: &str, allowed: &[&str], what: &str) -> Result<String> {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if allowed.contains(&ext.as_str()) {
        return Ok(ext);
    }
    let list = allowed
        .iter()
        .map(|a| format!("`.{a}`"))
        .collect::<Vec<_>>()
        .join(" or ");
    Err(Error::new(
        Code::PolDenied,
        format!(
            "A file holding {what} must be named {list}, so the tools that read it can find it."
        ),
    )
    .with_context(path.to_string()))
}

fn longest_backtick_run(s: &str) -> usize {
    let mut best = 0;
    let mut run = 0;
    for c in s.chars() {
        if c == '`' {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// Escape text for a context where HTML is not wanted. Used on the title only;
/// the body is HTML by definition and is written as supplied.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sandbox() -> (tempfile::TempDir, Workspace) {
        let t = tempfile::tempdir().expect("tempdir");
        let root = fs::canonicalize(t.path()).expect("canonicalize");
        let ws = Workspace::open(root).expect("workspace");
        (t, ws)
    }

    fn read(w: &Written) -> String {
        fs::read_to_string(w.path()).expect("read back")
    }

    #[test]
    fn all_three_operations_go_through_the_same_guard() {
        // The point of the crate. If one of them ever grows its own write, the
        // corpus stops testing it and every rule above becomes optional.
        let (_t, ws) = sandbox();
        let escape = "../escape";
        let a = create_file(
            &ws,
            &CreateFile {
                path: format!("{escape}.md"),
                body: "x".into(),
                expect: Expect::New,
            },
        )
        .unwrap_err();
        let b = create_diagram(
            &ws,
            &CreateDiagram {
                path: format!("{escape}.md"),
                mermaid: "flowchart TD\n  a-->b".into(),
                title: None,
                expect: Expect::New,
            },
        )
        .unwrap_err();
        let c = create_page(
            &ws,
            &CreatePage {
                path: format!("{escape}.html"),
                title: "t".into(),
                body: "<p>x</p>".into(),
                expect: Expect::New,
            },
        )
        .unwrap_err();
        for e in [a, b, c] {
            assert_eq!(e.code(), Code::FsPathEscapeBlocked);
        }
    }

    #[test]
    fn a_diagram_is_written_fenced_so_a_markdown_reader_renders_it() {
        let (_t, ws) = sandbox();
        let w = create_diagram(
            &ws,
            &CreateDiagram {
                path: "diagrams/flow.md".into(),
                mermaid: "flowchart TD\n  a-->b".into(),
                title: Some("Flow".into()),
                expect: Expect::New,
            },
        )
        .unwrap();
        assert_eq!(
            read(&w),
            "# Flow\n\n```mermaid\nflowchart TD\n  a-->b\n```\n"
        );
    }

    #[test]
    fn a_diagram_body_cannot_close_its_own_fence() {
        // Otherwise a diagram containing three backticks turns the rest of the
        // document into prose — and, in a document that is later indexed, into
        // text that reads like the agent's own commentary.
        let (_t, ws) = sandbox();
        let w = create_diagram(
            &ws,
            &CreateDiagram {
                path: "d.md".into(),
                mermaid: "flowchart TD\n  a[\"```\"]-->b".into(),
                title: None,
                expect: Expect::New,
            },
        )
        .unwrap();
        let text = read(&w);
        assert!(text.starts_with("````mermaid\n"), "{text}");
        assert!(text.ends_with("\n````\n"), "{text}");
    }

    #[test]
    fn an_mmd_file_holds_bare_source_because_a_fence_would_break_it() {
        let (_t, ws) = sandbox();
        let w = create_diagram(
            &ws,
            &CreateDiagram {
                path: "flow.mmd".into(),
                mermaid: "sequenceDiagram\n  A->>B: hi".into(),
                title: Some("ignored in bare source".into()),
                expect: Expect::New,
            },
        )
        .unwrap();
        assert_eq!(read(&w), "sequenceDiagram\n  A->>B: hi\n");
    }

    #[test]
    fn a_page_is_a_complete_document_with_its_title_escaped() {
        // The title is plain text from a caller that may have read it out of a
        // hostile file; `</title><script>` must not become markup.
        let (_t, ws) = sandbox();
        let w = create_page(
            &ws,
            &CreatePage {
                path: "report.html".into(),
                title: "Q2 <script>alert(1)</script>".into(),
                body: "<p>body</p>".into(),
                expect: Expect::New,
            },
        )
        .unwrap();
        let text = read(&w);
        assert!(text.starts_with("<!doctype html>"));
        assert!(text.contains("<title>Q2 &lt;script&gt;alert(1)&lt;/script&gt;</title>"));
        assert!(text.contains("<p>body</p>"));
        assert!(text.trim_end().ends_with("</html>"));
    }

    #[test]
    fn a_document_named_as_something_it_is_not_is_refused() {
        // An HTML page called `notes.rs` gets compiled; a diagram called
        // `notes.txt` is invisible to every renderer that would draw it.
        let (_t, ws) = sandbox();
        let e = create_page(
            &ws,
            &CreatePage {
                path: "src/notes.rs".into(),
                title: "t".into(),
                body: "<p>x</p>".into(),
                expect: Expect::New,
            },
        )
        .unwrap_err();
        assert_eq!(e.code(), Code::PolDenied);
        let e = create_diagram(
            &ws,
            &CreateDiagram {
                path: "notes.txt".into(),
                mermaid: "flowchart TD\n a-->b".into(),
                title: None,
                expect: Expect::New,
            },
        )
        .unwrap_err();
        assert_eq!(e.code(), Code::PolDenied);
    }

    #[test]
    fn an_empty_diagram_is_refused_rather_than_written_blank() {
        let (_t, ws) = sandbox();
        let e = create_diagram(
            &ws,
            &CreateDiagram {
                path: "d.md".into(),
                mermaid: "   \n".into(),
                title: None,
                expect: Expect::New,
            },
        )
        .unwrap_err();
        assert_eq!(e.code(), Code::PolDenied);
    }

    #[test]
    fn a_request_deserialises_from_the_json_an_mcp_handler_receives() {
        // The handler must stay a deserialize-call-serialize with no logic, so
        // the wire shape has to be the shape the function takes.
        let req: CreateFile = serde_json::from_str(
            r#"{"path":"notes/x.md","body":"hello","expect":{"replacing":"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"}}"#,
        )
        .unwrap();
        assert!(matches!(req.expect, Expect::Replacing(_)));
        // Omitted precondition means "this is a new file", never "overwrite".
        let req: CreateFile = serde_json::from_str(r#"{"path":"x.md","body":""}"#).unwrap();
        assert_eq!(req.expect, Expect::New);
    }
}
