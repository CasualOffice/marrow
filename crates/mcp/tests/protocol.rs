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
    let store = Store::open_with_migrations(
        dir.path().join("marrow.sqlite"),
        &[marrow_index::fts5::MIGRATION],
    )
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
