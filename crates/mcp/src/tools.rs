//! Tool definitions.
//!
//! Every tool is a thin wrapper over `marrow-query` ([LLD §10]): the handler
//! deserializes, calls, serializes. A handler containing an `if` that is not
//! error handling means the logic is in the wrong crate.
//!
//! # What the schemas are for
//!
//! An MCP tool schema is the only documentation the model gets. A parameter
//! whose description says "the query" teaches nothing; one that says what the
//! tool is *good at* and what it will *refuse* saves a round trip.
//!
//! [LLD §10]: ../../../docs/LLD.md

use serde_json::{json, Value};

/// One tool, as `tools/list` reports it.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: fn() -> Value,
}

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "search",
        description: "\
Search indexed local files. Returns ranked excerpts, each with the file path, a \
line span you can cite, and a structural breadcrumb showing where in the \
document it came from.

Matches on words, not substrings: `refresh_token` finds `refresh` and `token`.

Every result carries `provenance` (exact | degraded | approximate) and `origin`. \
A result with `origin: self_written` was produced by an agent and must not be \
cited as independent evidence.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Words to search for. Not a regular expression."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results. Default 20, cap 100.",
                        "minimum": 1,
                        "maximum": 100
                    },
                    "workspace": {
                        "type": "string",
                        "description": "Restrict to one workspace by name. Omit to search all."
                    },
                    "extension": {
                        "type": "string",
                        "description": "Restrict to one file extension, without the dot, e.g. `rs`."
                    },
                    "path_contains": {
                        "type": "string",
                        "description": "Restrict to paths containing this substring."
                    }
                },
                "required": ["query"]
            })
        },
    },
    Tool {
        name: "read_file",
        description: "\
Read an indexed file, or one region of it. Prefer a line range over the whole \
file: excerpts keep the answer citable and the context small.

Refuses files that are not indexed, and refuses cloud-only placeholder files — \
reading one would trigger a download of its contents.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the file." },
                    "start_line": {
                        "type": "integer",
                        "description": "First line to return, 1-based. Omit for the start.",
                        "minimum": 1
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "Last line to return, inclusive.",
                        "minimum": 1
                    }
                },
                "required": ["path"]
            })
        },
    },
    Tool {
        name: "file_info",
        description: "\
Everything Marrow knows about one file: its stable identity, content hash, \
previous paths if it has moved, version count, tier state, and index status.

Every fact names how it was derived. Facts Marrow cannot yet establish are \
reported as null rather than omitted, so absence is distinguishable from \
ignorance.",
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the file." }
                },
                "required": ["path"]
            })
        },
    },
    Tool {
        name: "list_workspaces",
        description: "\
List the folders Marrow has been granted, with file counts and index freshness. \
Use this first when a search returns nothing — the answer is often that the \
folder was never granted.",
        schema: || json!({ "type": "object", "properties": {} }),
    },
    Tool {
        name: "index_status",
        description: "\
Index health: how many files are indexed, how many are parsed into searchable \
chunks, and how many were deliberately skipped.

Cloud-only files are counted separately and are never read. A search that misses \
something is often explained here.",
        schema: || json!({ "type": "object", "properties": {} }),
    },
];

/// `tools/list` payload.
pub fn list() -> Value {
    json!({
        "tools": TOOLS.iter().map(|t| json!({
            "name": t.name,
            "description": t.description,
            "inputSchema": (t.schema)(),
        })).collect::<Vec<_>>()
    })
}

/// Look up a tool by the name a client sent.
pub fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_valid_object_schema() {
        for t in TOOLS {
            let s = (t.schema)();
            assert_eq!(
                s["type"],
                json!("object"),
                "{}: schema must be an object",
                t.name
            );
            assert!(s.get("properties").is_some(), "{}: no properties", t.name);
        }
    }

    #[test]
    fn every_required_parameter_is_actually_declared() {
        // A `required` naming a property that does not exist is accepted by
        // most validators and then fails at call time, which reads as a server
        // bug rather than a schema bug.
        for t in TOOLS {
            let s = (t.schema)();
            let props = s["properties"].as_object().expect("properties");
            if let Some(req) = s.get("required").and_then(|r| r.as_array()) {
                for name in req {
                    let name = name.as_str().expect("required entries are strings");
                    assert!(
                        props.contains_key(name),
                        "{}: required `{name}` is not a declared property",
                        t.name
                    );
                }
            }
        }
    }

    #[test]
    fn every_parameter_is_described() {
        // The schema is the only documentation the model gets.
        for t in TOOLS {
            let s = (t.schema)();
            for (name, spec) in s["properties"].as_object().expect("properties") {
                assert!(
                    spec.get("description").is_some(),
                    "{}: parameter `{name}` has no description",
                    t.name
                );
            }
        }
    }

    #[test]
    fn descriptions_say_what_a_tool_refuses() {
        // A tool that silently returns nothing teaches the model to distrust
        // it. The refusal has to be in the description.
        let search = find("search").unwrap();
        assert!(search.description.contains("self_written"));
        let read = find("read_file").unwrap();
        assert!(read.description.contains("Refuses"));
    }

    /// Names every parameter the dispatcher actually reads.
    ///
    /// Kept beside the schemas so the two move together. A parameter declared
    /// but ignored is the worst kind of bug: the schema says it works, the
    /// caller passes it, and nothing happens.
    const HANDLED: &[(&str, &[&str])] = &[
        (
            "search",
            &["query", "limit", "workspace", "extension", "path_contains"],
        ),
        ("read_file", &["path", "start_line", "end_line"]),
        ("file_info", &["path"]),
        ("list_workspaces", &[]),
        ("index_status", &[]),
    ];

    #[test]
    fn every_declared_parameter_is_actually_handled() {
        for t in TOOLS {
            let declared: Vec<String> = (t.schema)()["properties"]
                .as_object()
                .expect("properties")
                .keys()
                .cloned()
                .collect();
            let handled = HANDLED
                .iter()
                .find(|(n, _)| *n == t.name)
                .map(|(_, p)| *p)
                .unwrap_or_else(|| panic!("{}: not listed in HANDLED", t.name));
            for d in &declared {
                assert!(
                    handled.contains(&d.as_str()),
                    "{}: declares `{d}` but the dispatcher ignores it",
                    t.name
                );
            }
        }
    }

    #[test]
    fn tool_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for t in TOOLS {
            assert!(seen.insert(t.name), "duplicate tool name {}", t.name);
        }
    }

    #[test]
    fn lookup_finds_every_declared_tool_and_nothing_else() {
        for t in TOOLS {
            assert!(find(t.name).is_some());
        }
        assert!(find("delete_everything").is_none());
    }
}
