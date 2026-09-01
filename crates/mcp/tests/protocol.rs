//! End-to-end protocol tests against a real store and index.

use std::io::Cursor;

use marrow_core::Timestamp;
use marrow_mcp::{serve, Server};
use marrow_store::read::{NewRoot, NewWorkspace, StorageKind};
use marrow_store::Store;
use serde_json::{json, Value};

/// A store with one workspace, file-backed.
///
/// Not `open_in_memory`: that uses SQLite shared-cache, which locks at table
/// granularity rather than using WAL MVCC, so reader/writer behaviour differs
/// from what ships.
fn fixture() -> (tempfile::TempDir, Server) {
    let dir = tempfile::tempdir().unwrap();
    let store =
        Store::open_with_migrations(dir.path().join("marrow.sqlite"), marrow_index::MIGRATIONS)
            .unwrap();

    let now = Timestamp::now();
    let ws = store
        .upsert_workspace(NewWorkspace {
            workspace_id: marrow_core::WorkspaceId::new(),
            name: "notes".into(),
            at: now,
        })
        .unwrap();
    store
        .upsert_root(NewRoot {
            root_id: marrow_core::RootId::new(),
            workspace_id: ws,
            canonical_path: dir.path().to_string_lossy().into_owned(),
            volume_identity: None,
            grant_token: None,
            storage_kind: StorageKind::Local,
            cloud_provider: None,
            at: now,
        })
        .unwrap();
    store.flush().unwrap();

    let server = Server::new(store).unwrap();
    (dir, server)
}

/// Drive the server with newline-delimited requests, collect the responses.
fn exchange(server: &Server, requests: &[Value]) -> Vec<Value> {
    let input: String = requests
        .iter()
        .map(|r| format!("{r}\n"))
        .collect::<Vec<_>>()
        .join("");
    let mut out = Vec::new();
    serve(server, Cursor::new(input), &mut out).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("every line must be valid JSON"))
        .collect()
}

#[test]
fn initialize_reports_a_protocol_version_and_only_implemented_capabilities() {
    let (_d, s) = fixture();
    let r = exchange(&s, &[json!({"jsonrpc":"2.0","id":1,"method":"initialize"})]);
    assert_eq!(r.len(), 1);
    let result = &r[0]["result"];
    assert!(result["protocolVersion"].is_string());
    assert_eq!(result["serverInfo"]["name"], json!("marrow"));
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["capabilities"].get("resources").is_none());
}

#[test]
fn both_handshake_spellings_are_accepted() {
    // Part 2 §52 records that the spec renamed this; deployed clients did not.
    // Supporting both costs one match arm and avoids a server that is correct
    // by the document and useless in practice.
    let (_d, s) = fixture();
    for method in ["initialize", "server/discover"] {
        let r = exchange(&s, &[json!({"jsonrpc":"2.0","id":1,"method":method})]);
        assert!(
            r[0]["result"]["protocolVersion"].is_string(),
            "{method} was not handled"
        );
    }
}

#[test]
fn a_notification_gets_no_reply() {
    // Replying to a notification is a protocol violation, and clients that
    // enforce it will drop the connection.
    let (_d, s) = fixture();
    let r = exchange(
        &s,
        &[json!({"jsonrpc":"2.0","method":"notifications/initialized"})],
    );
    assert!(r.is_empty(), "got {r:?}");
}

#[test]
fn every_advertised_tool_can_actually_be_called() {
    // A schema that lists a tool the dispatcher does not handle fails at call
    // time and reads as a broken server.
    let (_d, s) = fixture();
    let listed = exchange(&s, &[json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})]);
    let names: Vec<String> = listed[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(!names.is_empty());

    for name in names {
        // `search` and the file tools need arguments; supply the minimum so a
        // missing-argument refusal is not mistaken for an unknown tool.
        let args = match name.as_str() {
            "search" => json!({ "query": "anything" }),
            "read_file" | "file_info" => json!({ "path": "/nonexistent" }),
            _ => json!({}),
        };
        let r = exchange(
            &s,
            &[json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call",
                "params": { "name": name, "arguments": args }
            })],
        );
        assert!(
            r[0].get("error").is_none(),
            "{name} returned a JSON-RPC error: {:?}",
            r[0]["error"]
        );
    }
}

#[test]
fn an_unknown_method_is_a_json_rpc_error_but_a_bad_argument_is_not() {
    // The distinction that matters: a protocol violation is an error; a tool
    // refusing a bad argument must reach the model as a message it can act on.
    let (_d, s) = fixture();

    let r = exchange(&s, &[json!({"jsonrpc":"2.0","id":1,"method":"no/such"})]);
    assert_eq!(r[0]["error"]["code"], json!(-32601));

    let r = exchange(
        &s,
        &[json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params": { "name": "search", "arguments": { "query": "" } }
        })],
    );
    assert!(r[0].get("error").is_none(), "must not be a protocol error");
    assert_eq!(r[0]["result"]["isError"], json!(true));
}

#[test]
fn malformed_json_does_not_kill_the_loop() {
    // One bad line from a client must not end the session.
    let (_d, s) = fixture();
    let input = "not json\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n";
    let mut out = Vec::new();
    serve(&s, Cursor::new(input), &mut out).unwrap();
    let lines: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["error"]["code"], json!(-32700));
    assert_eq!(lines[1]["id"], json!(7), "the loop continued");
}

#[test]
fn an_unknown_workspace_is_refused_rather_than_silently_empty() {
    // Silently returning nothing for a typo is indistinguishable from "nothing
    // matched", and a model will believe the second answer.
    let (_d, s) = fixture();
    let r = exchange(
        &s,
        &[json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params": { "name":"search", "arguments": { "query":"x", "workspace":"nope" } }
        })],
    );
    assert_eq!(r[0]["result"]["isError"], json!(true));
    let msg = r[0]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        msg.contains("nope"),
        "message must name the workspace: {msg}"
    );
}

#[test]
fn reading_an_unindexed_file_is_refused() {
    // This is not a general filesystem tool, and the workspace grant is what
    // says which files it may touch.
    let (_d, s) = fixture();
    let r = exchange(
        &s,
        &[json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params": { "name":"read_file", "arguments": { "path": "/etc/passwd" } }
        })],
    );
    assert_eq!(r[0]["result"]["isError"], json!(true));
}

#[test]
fn tool_results_carry_the_same_data_as_text_and_as_structure() {
    // Clients differ in which they read; a tool that returns prose to one and
    // data to the other has two formats to keep in step.
    let (_d, s) = fixture();
    let r = exchange(
        &s,
        &[json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params": { "name":"index_status", "arguments": {} }
        })],
    );
    let result = &r[0]["result"];
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(text).unwrap(),
        result["structuredContent"]
    );
}

#[test]
fn index_status_never_hides_the_cloud_only_count() {
    // A silent zero here is indistinguishable from "no cloud files", which is
    // the failure TIER-008 exists to prevent.
    let (_d, s) = fixture();
    let r = exchange(
        &s,
        &[json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params": { "name":"index_status", "arguments": {} }
        })],
    );
    let s = &r[0]["result"]["structuredContent"];
    assert!(
        s.get("cloud_only_not_read").is_some(),
        "the count must always be present, even at zero"
    );
}

// ── write tools ───────────────────────────────────────────────────────────

/// The `origin = SELF` rule, at the seam where it is easiest to skip.
///
/// `marrow-tools` returns `origin = SELF` and the ingest pipeline reads it back
/// from `self_written` — but only if something wrote that row. A handler that
/// forgets is the whole failure: the write succeeds, the model is told it is
/// uncitable, and the next scan disagrees.
mod writes {
    use super::*;
    use serde_json::json;

    /// Like `fixture`, but keeps the database path so a test can read back
    /// what the handler recorded. The `Store` itself moves into the server —
    /// there is one writer, and a second would be a second writer.
    struct WriteFixture {
        _dir: tempfile::TempDir,
        root: std::path::PathBuf,
        db: std::path::PathBuf,
        server: Server,
    }

    impl WriteFixture {
        /// A read-only connection, for asserting on what the handler wrote.
        fn read(&self) -> rusqlite::Connection {
            rusqlite::Connection::open(&self.db).unwrap()
        }
    }

    fn fixture_with_root() -> WriteFixture {
        let (dir, server) = fixture();
        let root = dir.path().to_path_buf();
        let db = root.join("marrow.sqlite");
        WriteFixture {
            _dir: dir,
            root,
            db,
            server,
        }
    }

    fn call(server: &Server, name: &str, args: serde_json::Value) -> serde_json::Value {
        let out = exchange(
            server,
            &[json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": args }
            })],
        );
        out.into_iter().next().expect("a call gets a response")
    }

    fn is_error(v: &serde_json::Value) -> bool {
        v.pointer("/result/isError").and_then(|b| b.as_bool()) == Some(true)
    }

    fn text(v: &serde_json::Value) -> String {
        v.pointer("/result/content/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn a_created_file_is_recorded_as_self_written_not_merely_reported_as_such() {
        let f = fixture_with_root();
        let out = call(
            &f.server,
            "create_file",
            json!({ "path": "notes/summary.md", "body": "# Summary\n\nAs concluded.\n" }),
        );
        assert!(!is_error(&out), "{}", text(&out));
        assert!(text(&out).contains("self_written"), "{}", text(&out));

        // The row is the point. Without it the next scan calls this the user's
        // own work and it becomes citable.
        let conn = f.read();
        let (n, tool): (i64, String) = conn
            .query_row(
                "SELECT count(*), COALESCE(max(tool), '') FROM self_written",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 1, "the write was not recorded");
        assert_eq!(tool, "create_file");
    }

    #[test]
    fn creating_over_an_existing_file_is_refused_with_the_reason() {
        let f = fixture_with_root();
        std::fs::write(f.root.join("taken.md"), "the user's work").unwrap();
        let out = call(
            &f.server,
            "create_file",
            json!({ "path": "taken.md", "body": "mine now" }),
        );
        assert!(is_error(&out));
        assert!(text(&out).contains("already exists"), "{}", text(&out));
        assert_eq!(
            std::fs::read_to_string(f.root.join("taken.md")).unwrap(),
            "the user's work",
            "the file was overwritten anyway"
        );
    }

    #[test]
    fn a_path_that_leaves_the_workspace_is_refused() {
        let f = fixture_with_root();
        let out = call(
            &f.server,
            "create_file",
            json!({ "path": "../escaped.md", "body": "x" }),
        );
        assert!(is_error(&out));
        assert!(!f.root.parent().unwrap().join("escaped.md").exists());
    }

    #[test]
    fn a_diagram_must_actually_be_a_diagram() {
        let f = fixture_with_root();
        let out = call(
            &f.server,
            "create_diagram",
            json!({ "path": "flow.md", "mermaid": "just some prose" }),
        );
        assert!(is_error(&out), "{}", text(&out));
    }

    #[test]
    fn a_fetch_that_needs_confirmation_is_refused_rather_than_silently_granted() {
        // There is no one to ask over MCP. Treating that as consent would make
        // the confirmation rule decorative.
        let f = fixture_with_root();
        let out = call(
            &f.server,
            "fetch_url",
            json!({ "url": "https://example.com/" }),
        );
        assert!(is_error(&out));
        assert!(text(&out).contains("cannot"), "{}", text(&out));
    }

    /// **A refusal has to leave something to do.**
    ///
    /// Consent was built fresh and empty inside the handler, so `decide`
    /// returned `NewHost` for every URL and the tool refused everything, on
    /// every host, forever — while its description advertised a one-time
    /// confirmation step. A model spent a call discovering a tool that could
    /// never succeed, and the message ended at "no way to ask", which is true
    /// and actionless.
    #[test]
    fn a_host_the_user_allowed_is_no_longer_treated_as_new() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(
            dir.path().join("net-allow.txt"),
            "# hosts I have agreed to\nexample.com\n",
        )
        .expect("write allowlist");

        let store = Store::open_with_migrations(
            dir.path().join("allowed.sqlite"),
            marrow_index::MIGRATIONS,
        )
        .expect("store");
        let server = Server::new(store)
            .expect("server")
            .with_data_dir(dir.path());

        let out = call(
            &server,
            "fetch_url",
            json!({ "url": "https://example.com/" }),
        );
        let msg = text(&out);
        assert!(
            !msg.contains("not fetched from example.com before"),
            "an allowed host was still treated as new: {msg}"
        );

        // And the refusal for a host that is *not* listed must name the file
        // and the line to add, rather than stopping at "cannot ask".
        let other = call(
            &server,
            "fetch_url",
            json!({ "url": "https://other.invalid/" }),
        );
        let msg = text(&other);
        assert!(is_error(&other));
        assert!(
            msg.contains("net-allow.txt"),
            "the refusal must say where to grant it: {msg}"
        );
    }

    #[test]
    fn a_plain_http_url_is_refused_without_reaching_the_network() {
        let f = fixture_with_root();
        let out = call(
            &f.server,
            "fetch_url",
            json!({ "url": "http://127.0.0.1:80/" }),
        );
        assert!(is_error(&out));
    }

    #[test]
    fn every_write_tool_says_what_it_refuses_and_what_it_costs() {
        // An MCP schema is the only documentation the model gets. A tool that
        // writes to the user's disk or sends a request off the machine and does
        // not say so in its description is a trap.
        for t in marrow_mcp::tools::WRITE_TOOLS {
            assert!(
                t.description.contains("Refuses") || t.description.contains("refus"),
                "{} does not say what it refuses",
                t.name
            );
        }
        let fetch = marrow_mcp::tools::WRITE_TOOLS
            .iter()
            .find(|t| t.name == "fetch_url")
            .unwrap();
        assert!(
            fetch.description.contains("off the machine"),
            "fetch_url must say that it is egress"
        );
        for t in marrow_mcp::tools::WRITE_TOOLS
            .iter()
            .filter(|t| t.name != "fetch_url")
        {
            assert!(
                t.description.contains("evidence") || t.description.contains("cite"),
                "{} must say its output cannot be cited",
                t.name
            );
        }
    }
}

/// `search_literal` — the escape hatch, over MCP.
///
/// Every test here pins a reason the tool exists rather than the shape of its
/// output. The index tokenizes; this reads bytes; and because it reads bytes
/// it inherits the two rules that make reading safe — never open a cloud
/// placeholder, and never let what this system wrote count as corroboration.
mod literal {
    use super::*;
    use marrow_core::{ContentHash, FileId, FileStatus, Origin, TierState};
    use marrow_store::{NewFile, NewVersion};

    /// One workspace, and files actually written to disk so a scan can read
    /// them. Returns the directory so paths can be asserted against.
    fn scannable(files: &[(&str, &str, TierState, Origin)]) -> (tempfile::TempDir, Server) {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open_with_migrations(dir.path().join("marrow.sqlite"), marrow_index::MIGRATIONS)
                .unwrap();
        let now = Timestamp::now();
        let ws = store
            .upsert_workspace(NewWorkspace {
                workspace_id: marrow_core::WorkspaceId::new(),
                name: "notes".into(),
                at: now,
            })
            .unwrap();
        let root = store
            .upsert_root(NewRoot {
                root_id: marrow_core::RootId::new(),
                workspace_id: ws,
                canonical_path: dir.path().to_string_lossy().into_owned(),
                volume_identity: None,
                grant_token: None,
                storage_kind: StorageKind::Local,
                cloud_provider: None,
                at: now,
            })
            .unwrap();

        for (name, body, tier, origin) in files {
            let path = dir.path().join(name);
            std::fs::write(&path, body).unwrap();
            let file = FileId::new();
            let f = NewFile {
                file_id: file,
                workspace_id: ws,
                root_id: root,
                current_path: Some(path.to_string_lossy().into_owned()),
                fs_identity: Some(name.to_string()),
                tier_state: *tier,
                origin: *origin,
                origin_txn_id: None,
                external_source_url: None,
                status: FileStatus::Active,
                at: now,
            };
            let v = NewVersion::new(
                file,
                *name,
                body.len() as i64,
                ContentHash::of(body.as_bytes()),
            );
            store
                .writer()
                .submit(move |c| {
                    marrow_store::read::insert_file_with_version(c, &f, &v).map(|_| ())
                })
                .unwrap();
        }
        store.flush().unwrap();
        (dir, Server::new(store).unwrap())
    }

    fn call(server: &Server, args: Value) -> Value {
        let out = exchange(
            server,
            &[json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call",
                "params": { "name":"search_literal", "arguments": args }
            })],
        );
        let r = out.into_iter().next().expect("a call gets a response");
        let text = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {r}"))
            .to_string();
        serde_json::from_str(&text).expect("the payload must be JSON")
    }

    /// **The whole reason the tool exists.** FTS5 tokenizes, so a pattern that
    /// is punctuation cannot be expressed as a word query at all — and the
    /// zero-results path has been suggesting a literal scan since M1.
    #[test]
    fn a_pattern_the_word_index_cannot_express_is_found_by_reading_the_bytes() {
        let (_d, s) = scannable(&[(
            "app.rs",
            "fn main() {\n    todo!(\"TODO(sachin): });\");\n}\n",
            TierState::Resident,
            Origin::User,
        )]);
        let v = call(&s, json!({ "pattern": "});" }));
        assert_eq!(v["matches"], json!(1), "{v}");
        assert_eq!(v["results"][0]["line"], json!(2));
        assert_eq!(v["results"][0]["provenance"], json!("exact"));
        assert_eq!(v["coverage"]["complete"], json!(true));
    }

    /// **Never hydrate a placeholder.** The file is not on this disk; opening it is what
    /// downloads it. It must be skipped unread *and* counted — a scan that
    /// quietly omitted the cloud-only half of a folder would be the most
    /// misleading possible "no matches".
    #[test]
    fn a_cloud_only_file_is_skipped_unread_and_counted() {
        let (_d, s) = scannable(&[
            ("here.md", "needle\n", TierState::Resident, Origin::User),
            ("cloud.md", "needle\n", TierState::Placeholder, Origin::User),
        ]);
        let v = call(&s, json!({ "pattern": "needle" }));
        assert_eq!(v["matches"], json!(1), "the placeholder must not be read");
        assert_eq!(v["coverage"]["files_skipped_cloud_only"], json!(1));
        assert_eq!(
            v["coverage"]["complete"],
            json!(false),
            "a scan that skipped a file has not covered its scope: {v}"
        );
    }

    /// **The `origin = SELF` rule.** A hit inside a file this system wrote is not
    /// independent corroboration, and the payload says so rather than leaving
    /// the caller to work it out from the path.
    #[test]
    fn a_hit_in_a_file_this_system_wrote_is_not_citable() {
        let (_d, s) = scannable(&[
            (
                "mine.md",
                "the finding\n",
                TierState::Resident,
                Origin::User,
            ),
            (
                "agent.md",
                "the finding\n",
                TierState::Resident,
                Origin::SelfWritten,
            ),
        ]);
        let v = call(&s, json!({ "pattern": "the finding" }));
        assert_eq!(v["matches"], json!(2));
        let by_origin: std::collections::HashMap<&str, bool> = v["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["origin"].as_str().unwrap(),
                    r["citable"].as_bool().unwrap(),
                )
            })
            .collect();
        assert_eq!(by_origin.get("user"), Some(&true));
        assert_eq!(
            by_origin.get("self_written"),
            Some(&false),
            "a self-written hit must be marked uncitable: {v}"
        );
    }

    /// A partial scan must never read as a complete one. `matches: 0` with no
    /// coverage block is how a model concludes a string is absent from a disk
    /// it only looked at a tenth of.
    #[test]
    fn stopping_early_is_reported_rather_than_looking_like_an_exhaustive_answer() {
        let (_d, s) = scannable(&[
            ("a.md", "hit\nhit\nhit\n", TierState::Resident, Origin::User),
            ("b.md", "hit\nhit\nhit\n", TierState::Resident, Origin::User),
        ]);
        let v = call(&s, json!({ "pattern": "hit", "limit": 2 }));
        assert_eq!(v["matches"], json!(2));
        assert_eq!(v["coverage"]["stopped_because"], json!("match_limit"));
        assert_eq!(v["coverage"]["complete"], json!(false));
        assert!(
            v["coverage"]["advice"]
                .as_str()
                .unwrap()
                .contains("does not mean"),
            "the advice must say what an absent match does not prove: {v}"
        );
    }

    /// Narrowing is the documented fix for an incomplete scan, so it has to
    /// actually narrow — and `path_contains` is caller input going into SQL.
    #[test]
    fn narrowing_by_path_reduces_the_scope_rather_than_filtering_the_results() {
        let (_d, s) = scannable(&[
            ("keep.md", "needle\n", TierState::Resident, Origin::User),
            ("drop.md", "needle\n", TierState::Resident, Origin::User),
        ]);
        let v = call(&s, json!({ "pattern": "needle", "path_contains": "keep" }));
        assert_eq!(v["matches"], json!(1));
        assert_eq!(
            v["coverage"]["files_in_scope"],
            json!(1),
            "the filter must shrink the scan, not the result list: {v}"
        );
        // A quote in the fragment must not end the statement it is bound into.
        let v = call(
            &s,
            json!({ "pattern": "needle", "path_contains": "' OR 1=1 --" }),
        );
        assert_eq!(v["matches"], json!(0));
        assert_eq!(v["coverage"]["files_in_scope"], json!(0));
    }

    #[test]
    fn an_empty_pattern_is_refused_rather_than_matching_everything() {
        let (_d, s) = scannable(&[("a.md", "x\n", TierState::Resident, Origin::User)]);
        let out = exchange(
            &s,
            &[json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call",
                "params": { "name":"search_literal", "arguments": { "pattern": "  " } }
            })],
        );
        assert_eq!(out[0]["result"]["isError"], json!(true));
    }

    #[test]
    fn a_regex_is_only_a_regex_when_asked_for() {
        let (_d, s) = scannable(&[(
            "a.md",
            "literal a.c here\nand abc there\n",
            TierState::Resident,
            Origin::User,
        )]);
        // `.` is a character, not a wildcard, unless regex is set.
        let plain = call(&s, json!({ "pattern": "a.c" }));
        assert_eq!(plain["matches"], json!(1));
        assert_eq!(plain["results"][0]["line"], json!(1));

        let re = call(&s, json!({ "pattern": "a.c", "regex": true }));
        assert_eq!(re["matches"], json!(2), "{re}");
    }
}

/// The read tools against an index that actually has content in it.
///
/// `fixture` has a workspace and no files; `literal::scannable` has files on
/// disk but nothing in the lexical index or the chunk table. Every claim these
/// tools make about what is *searchable* needs all three, and the absence of a
/// fixture carrying all three is why nothing here was ever tested.
mod indexed {
    use super::*;
    use marrow_core::{
        ChunkId, ContentHash, FileId, FileStatus, Origin, ProvenanceClass, SourceSpan, TierState,
    };
    use marrow_index::{Fts5Index, TextDoc, TextIndex};
    use marrow_store::read::NewChunk;
    use marrow_store::{NewFile, NewVersion};

    /// One file as the fixture takes it: name, contents, and whether its text
    /// was extracted. `false` is a photo — recorded, findable by name, with
    /// nothing to search.
    struct Doc<'a> {
        name: &'a str,
        body: &'a str,
        parsed: bool,
    }

    fn doc<'a>(name: &'a str, body: &'a str, parsed: bool) -> Doc<'a> {
        Doc { name, body, parsed }
    }

    /// Files on disk, in `files`/`file_versions`, and — when `parsed` — in the
    /// chunk table and the lexical index too.
    fn corpus(docs: &[Doc<'_>]) -> (tempfile::TempDir, Server) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            Store::open_with_migrations(dir.path().join("marrow.sqlite"), marrow_index::MIGRATIONS)
                .expect("store");
        let now = Timestamp::now();
        let ws = store
            .upsert_workspace(NewWorkspace {
                workspace_id: marrow_core::WorkspaceId::new(),
                name: "notes".into(),
                at: now,
            })
            .expect("workspace");
        let root = store
            .upsert_root(NewRoot {
                root_id: marrow_core::RootId::new(),
                workspace_id: ws,
                canonical_path: dir.path().to_string_lossy().into_owned(),
                volume_identity: None,
                grant_token: None,
                storage_kind: StorageKind::Local,
                cloud_provider: None,
                at: now,
            })
            .expect("root");

        let index = Fts5Index::open(&store).expect("index");
        for d in docs {
            let path = dir.path().join(d.name);
            std::fs::write(&path, d.body).expect("write");
            let path = path.to_string_lossy().into_owned();
            let file = FileId::new();
            let f = NewFile {
                file_id: file,
                workspace_id: ws,
                root_id: root,
                current_path: Some(path.clone()),
                fs_identity: Some(d.name.to_string()),
                tier_state: TierState::Resident,
                origin: Origin::User,
                origin_txn_id: None,
                external_source_url: None,
                status: FileStatus::Active,
                at: now,
            };
            let v = NewVersion::new(
                file,
                d.name,
                d.body.len() as i64,
                ContentHash::of(d.body.as_bytes()),
            );
            let version = v.version_id;
            store
                .writer()
                .submit(move |c| {
                    marrow_store::read::insert_file_with_version(c, &f, &v).map(|_| ())
                })
                .expect("insert");
            if !d.parsed {
                continue;
            }
            let chunk = ChunkId::new();
            let text = d.body.to_string();
            let hash = ContentHash::of(d.body.as_bytes());
            store
                .writer()
                .submit(move |c| {
                    marrow_store::read::replace_chunks(
                        c,
                        version,
                        &[NewChunk {
                            chunk_id: chunk,
                            version_id: version,
                            chunk_kind: "TEXT".into(),
                            text,
                            context_prefix: None,
                            token_count: 8,
                            text_hash: hash,
                            chunker_version: "test".into(),
                            provenance_class: "EXACT".into(),
                            source_span: None,
                        }],
                    )
                })
                .expect("chunks");
            index
                .upsert(&[TextDoc {
                    chunk_id: chunk,
                    file_id: file,
                    version_id: version,
                    workspace_id: ws,
                    path,
                    title: String::new(),
                    body: d.body.to_string(),
                    span: SourceSpan::Lines { start: 1, end: 1 },
                    provenance: ProvenanceClass::Exact,
                    origin: Origin::User,
                    modified: now,
                }])
                .expect("index");
        }
        store.flush().expect("flush");
        (dir, Server::new(store).expect("server"))
    }

    fn call(server: &Server, tool: &str, args: Value) -> Value {
        let out = exchange(
            server,
            &[json!({
                "jsonrpc":"2.0","id":1,"method":"tools/call",
                "params": { "name": tool, "arguments": args }
            })],
        );
        out.into_iter().next().expect("a call gets a response")
    }

    fn payload(v: &Value) -> Value {
        let text = v["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {v}"));
        serde_json::from_str(text).expect("the payload must be JSON")
    }

    /// A lease that "renews", and a question that says "renew".
    fn lease() -> (tempfile::TempDir, Server) {
        corpus(&[
            doc(
                "lease.md",
                "The tenant renews this agreement every January.\n",
                true,
            ),
            doc("holiday.jpg", "\u{fffd}not text\n", false),
        ])
    }

    /// **C12.** The document is right there and the old default returned
    /// nothing, because `MatchMode::Terms` wanted a file containing *when*,
    /// *does*, *the*, *lease* and *renew* — and the lease says "renews".
    #[test]
    fn a_natural_language_question_finds_the_document_that_answers_it() {
        let (_d, s) = lease();
        let v = payload(&call(
            &s,
            "search",
            json!({ "query": "when does the lease renew" }),
        ));
        assert_eq!(v["match"], json!("any"), "the default must be `any`: {v}");
        assert_eq!(v["total"], json!(1), "{v}");
        assert!(v["results"][0]["path"]
            .as_str()
            .expect("a path")
            .ends_with("lease.md"));
    }

    /// The other half of the same fact: conjunction is still available, and it
    /// is still the mode that answers nothing here.
    #[test]
    fn requiring_every_word_is_asked_for_rather_than_assumed() {
        let (_d, s) = lease();
        let all = payload(&call(
            &s,
            "search",
            json!({ "query": "when does the lease renew", "match": "all" }),
        ));
        assert_eq!(all["match"], json!("all"));
        assert_eq!(all["total"], json!(0), "{all}");

        let phrase = payload(&call(
            &s,
            "search",
            json!({ "query": "renews this agreement", "match": "phrase" }),
        ));
        assert_eq!(phrase["match"], json!("phrase"));
        assert_eq!(phrase["total"], json!(1), "{phrase}");
    }

    /// A mode this build does not have must not quietly become the default.
    /// Silently running a different query than the caller asked for is how a
    /// caller believes it constrained a search that it did not.
    /// **`search` goes through the same retrieval every other surface uses.**
    ///
    /// It called `index.search` directly and numbered the raw FTS5 order, so
    /// the §113.3 multipliers never applied here: an agent-written file came
    /// back flagged `citable: false` and still ranked wherever BM25 put it,
    /// and a degraded-provenance chunk outranked an exact one. The CLI and the
    /// desktop both down-weight those, so the same query against the same
    /// index came back in a different order depending on which surface asked.
    ///
    /// `branches` is the cheap proof that it now goes through `search_hybrid`:
    /// nothing else populates it. The multipliers themselves are tested where
    /// they live, in `marrow-query`.
    #[test]
    fn a_search_says_which_branches_ran_because_it_goes_through_the_shared_path() {
        let (_d, s) = lease();
        let out = call(&s, "search", json!({ "query": "lease" }));
        let payload: Value =
            serde_json::from_str(out["result"]["content"][0]["text"].as_str().expect("text"))
                .expect("json");
        // Lexical only, and it says so: the semantic branch needs an embedder
        // and this is a stdio server that must start instantly. A caller that
        // assumed fusion because the product advertises it elsewhere would be
        // drawing a conclusion this run does not support.
        assert_eq!(payload["branches"], json!(["lexical"]));
    }

    /// **The surface `table compute` exists for.** Over MCP the caller is a
    /// model, and a model handed forty numbers as text will usually add them
    /// correctly and cannot tell anyone when it did not. Shipping the feature
    /// to the CLI alone left the one surface where it matters reading the
    /// numbers out instead.
    #[test]
    fn compute_table_is_reachable_and_refuses_a_file_that_is_not_indexed() {
        let (_d, s) = lease();
        let out = call(
            &s,
            "compute_table",
            json!({ "path": "/nowhere/ledger.xlsx", "range": "B1:B9" }),
        );
        assert_eq!(out["result"]["isError"], json!(true));
        let msg = out["result"]["content"][0]["text"]
            .as_str()
            .expect("a message");
        assert!(
            msg.contains("not indexed"),
            "must say why, not just fail: {msg}"
        );
    }

    #[test]
    fn an_unknown_match_mode_is_refused_and_names_the_ones_that_exist() {
        let (_d, s) = lease();
        let out = call(&s, "search", json!({ "query": "lease", "match": "fuzzy" }));
        assert_eq!(out["result"]["isError"], json!(true));
        let msg = out["result"]["content"][0]["text"]
            .as_str()
            .expect("a message");
        assert!(msg.contains("fuzzy"), "must name what was sent: {msg}");
        for mode in ["any", "all", "phrase"] {
            assert!(msg.contains(mode), "must name `{mode}`: {msg}");
        }
    }

    /// **F8.** `files_indexed: 3` beside `searchable_chunks: 1` reads as "all
    /// three are searchable". Only one of them has any text in the index, and
    /// the payload has to say which number is which.
    #[test]
    fn index_status_separates_what_is_searchable_from_what_is_merely_indexed() {
        let (_d, s) = corpus(&[
            doc("notes.md", "the finding\n", true),
            doc("a.jpg", "binary\n", false),
            doc("b.jpg", "binary\n", false),
        ]);
        let v = payload(&call(&s, "index_status", json!({})));
        assert_eq!(v["files_indexed"], json!(3), "{v}");
        assert_eq!(v["files_searchable"], json!(1), "{v}");

        let not = &v["files_not_searchable"];
        assert_eq!(not["total"], json!(2), "{v}");
        // The three reasons must account for the total exactly, or the payload
        // shows a number nobody can explain.
        let parts: i64 = ["no_parser", "parse_failed", "not_processed"]
            .iter()
            .map(|k| {
                not[*k]
                    .as_i64()
                    .unwrap_or_else(|| panic!("{k} missing: {v}"))
            })
            .sum();
        assert_eq!(parts, 2, "the reasons must sum to the total: {v}");
    }

    /// The description promised "how many were deliberately skipped" and the
    /// payload had no such field. Every one of these is a claim the
    /// description now makes, so every one has to exist even at zero.
    #[test]
    fn index_status_returns_every_field_its_description_promises() {
        let (_d, s) = corpus(&[doc("notes.md", "the finding\n", true)]);
        let v = payload(&call(&s, "index_status", json!({})));
        for field in [
            "files_indexed",
            "files_searchable",
            "files_not_searchable",
            "searchable_chunks",
            "cloud_only_not_read",
            "content_bytes",
            "workspaces",
            "schema_version",
            "last_indexed_ms",
            "watcher",
            "may_be_stale",
            "freshness",
        ] {
            assert!(v.get(field).is_some(), "`{field}` is promised: {v}");
        }
    }

    /// **C17.** "with file counts and index freshness" — and the payload had
    /// no freshness field of any kind. It must also be the *same* freshness
    /// `index_status` reports, or two tools answer one question differently.
    #[test]
    fn list_workspaces_returns_the_freshness_it_promises_and_agrees_with_index_status() {
        let (_d, s) = corpus(&[doc("notes.md", "the finding\n", true)]);
        let ws = payload(&call(&s, "list_workspaces", json!({})));
        let st = payload(&call(&s, "index_status", json!({})));
        for field in ["last_indexed_ms", "watcher", "may_be_stale", "freshness"] {
            assert!(ws.get(field).is_some(), "`{field}` is promised: {ws}");
            assert_eq!(
                ws[field], st[field],
                "`{field}` differs between the two tools"
            );
        }
        assert_eq!(ws["workspaces"][0]["name"], json!("notes"), "{ws}");
    }

    /// **F9.** The index says a file is here; the disk says it is gone.
    ///
    /// `read_file` found out because it opened the file. `file_info` never
    /// touched the disk, so it answered `citable: true, indexed_for_search:
    /// true, tier_state: "resident"` about a file that no longer exists —
    /// which is the one question an agent calls it to settle.
    #[test]
    fn file_info_does_not_call_a_file_that_is_gone_citable() {
        let (dir, s) = corpus(&[doc("notes.md", "the finding\n", true)]);
        let path = dir.path().join("notes.md");

        let before = payload(&call(&s, "file_info", json!({ "path": path })));
        assert_eq!(before["present_on_disk"], json!(true));
        assert_eq!(before["citable"], json!(true));
        assert_eq!(before["indexed_for_search"], json!(true));
        assert_eq!(before["tier_state"], json!("resident"));
        assert_eq!(
            before["note"],
            Value::Null,
            "nothing to warn about: {before}"
        );

        // The index is not reconciled: the row still says ACTIVE and RESIDENT,
        // which is exactly the window this bug lived in.
        std::fs::remove_file(&path).expect("remove");

        let after = payload(&call(&s, "file_info", json!({ "path": path })));
        assert_eq!(after["present_on_disk"], json!(false), "{after}");
        assert_eq!(after["citable"], json!(false), "{after}");
        assert_eq!(after["indexed_for_search"], json!(false), "{after}");
        assert_eq!(after["tier_state"], json!("missing"), "{after}");
        assert_eq!(
            after["recorded_tier_state"],
            json!("resident"),
            "what the last scan saw is still a fact worth keeping: {after}"
        );
    }

    /// Reported, not refused — and the reason is what is still in the payload.
    /// A refusal would read as "no such path in the index", and would discard
    /// the hash and the path history that say whether the content moved.
    #[test]
    fn a_file_that_is_gone_keeps_the_metadata_that_says_where_it_went() {
        let (dir, s) = corpus(&[doc("notes.md", "the finding\n", true)]);
        let path = dir.path().join("notes.md");
        std::fs::remove_file(&path).expect("remove");

        let out = call(&s, "file_info", json!({ "path": path }));
        assert!(
            out["result"]["isError"].as_bool() != Some(true),
            "must be answered, not refused: {out}"
        );
        let v = payload(&out);
        assert!(v["content_hash"].is_string(), "{v}");
        assert!(v["file_id"].is_string(), "{v}");
        assert!(v.get("previous_paths").is_some(), "{v}");
        let note = v["note"].as_str().unwrap_or_else(|| panic!("a note: {v}"));
        assert!(
            note.contains("previous_paths") && note.contains("marrow index"),
            "the note must name a cause and an action: {note}"
        );
    }

    /// The two tools have to agree. `read_file` reporting "gone" while
    /// `file_info` reports "resident, indexed, citable" about the same path is
    /// the contradiction that made this worth fixing.
    #[test]
    fn file_info_and_read_file_agree_about_a_file_that_is_gone() {
        let (dir, s) = corpus(&[doc("notes.md", "the finding\n", true)]);
        let path = dir.path().join("notes.md");
        std::fs::remove_file(&path).expect("remove");

        let read = call(&s, "read_file", json!({ "path": path }));
        assert_eq!(read["result"]["isError"], json!(true), "{read}");

        let info = payload(&call(&s, "file_info", json!({ "path": path })));
        assert_eq!(info["citable"], json!(false), "{info}");
    }

    /// **A limit the schema declares and the code ignores is decorative.**
    ///
    /// `minimum: 1, maximum: 100` were both advertised and both clamped, so
    /// asking for 100,000 returned 100 and asking for 0 returned one — neither
    /// what was requested, neither reported. A caller that receives fewer rows
    /// than it asked for and no error draws the wrong conclusion about the
    /// corpus.
    #[test]
    fn a_limit_outside_the_advertised_bounds_is_refused_not_quietly_adjusted() {
        let (_d, s) = corpus(&[doc("a.md", "needle", true)]);
        for bad in [json!(0), json!(100_000)] {
            let out = call(&s, "search", json!({ "query": "needle", "limit": bad }));
            assert_eq!(
                out["result"]["isError"],
                json!(true),
                "limit {bad} was silently adjusted instead of refused"
            );
            let msg = out["result"]["content"][0]["text"].as_str().unwrap_or("");
            assert!(
                msg.contains("between 1 and"),
                "the refusal must name the bounds: {msg}"
            );
        }
    }

    /// **`total` is what came back, not what exists.**
    ///
    /// A caller asking for N and receiving N cannot tell a corpus with exactly
    /// N matches from one with thousands. "These are the results" and "these
    /// are the first N" are different claims.
    #[test]
    fn a_full_page_says_there_may_be_more() {
        let (_d, s) = corpus(&[
            doc("a.md", "needle one", true),
            doc("b.md", "needle two", true),
        ]);
        let full = payload(&call(
            &s,
            "search",
            json!({ "query": "needle", "limit": 1 }),
        ));
        assert_eq!(full["total"], json!(1));
        assert_eq!(
            full["more_available"],
            json!(true),
            "a page that filled its limit must say so: {full}"
        );

        let all = payload(&call(
            &s,
            "search",
            json!({ "query": "needle", "limit": 10 }),
        ));
        assert_eq!(
            all["more_available"],
            json!(false),
            "a page with room to spare must not claim there is more: {all}"
        );
    }
}
