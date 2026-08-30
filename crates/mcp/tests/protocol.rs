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

/// Invariant #9, at the seam where it is easiest to skip.
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
        assert!(text(&out).contains("no way to ask"), "{}", text(&out));
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

    /// **Invariant #5.** The file is not on this disk; opening it is what
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

    /// **Invariant #9.** A hit inside a file this system wrote is not
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
