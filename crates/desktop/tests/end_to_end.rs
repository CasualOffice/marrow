//! The whole product, once: index real files, ask a real model, check the
//! answer came from the files.
//!
//! Everything else in this repository tests one layer. This tests the seam
//! between all of them, which is where the last three sessions' worth of bugs
//! actually lived — a fake timeout, a missing chat template, a delimiter that
//! defeated the cache. None of those were visible from inside a unit test.
//!
//! `#[ignore]`d because it wants a model on disk and about ten seconds:
//!
//! ```text
//! cargo test -p marrow-desktop --test end_to_end -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use marrow_core::{RootId, Timestamp, WorkspaceId};
use marrow_desktop::{ask, Core, Hub};
use marrow_ingest::{ingest_root_with_index, IngestPolicy, Progress};
// Two crates each have a `Cancel`; they are not the same type and the compiler
// is right to say so.
use marrow_ingest::Cancel as IngestCancel;
use marrow_model::queue::Cancel;
use marrow_scan::AuthorizedRoot;
use marrow_store::{NewRoot, NewWorkspace, StorageKind, Store};

/// A small corpus with one fact in it that no model could know.
///
/// The date and the figure are deliberately arbitrary. If the answer contains
/// them, it read the file; if it contains something plausible instead, it
/// recalled a lease from its training data and the whole pipeline is a
/// confident liar.
const LEASE: &str = "\
# Unit 7B, Harbour Works

The agreement runs from 1 January 2024 and renews on **31 December 2031**
unless either party gives notice in writing ninety days before the end of the
then-current term.

Rent is 2,417 per calendar month, payable in advance on the first working day.
It is reviewed each January against the published index, with any increase
capped at four per cent.
";

/// Deliberately shares no words with the question that must find it.
const PARAPHRASE: &str = "\
# Unit 7B, rolling over

The tenancy continues automatically at the conclusion of each successive term
unless either side serves written notice within the stipulated window.
";

const HANDBOOK: &str = "\
# Deliveries

Deliveries are accepted between 07:00 and 11:00 on weekdays only. The loading
bay is shared with Unit 7A and must be left clear.
";

fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".local/share/marrow")
}

/// An empty, authorized workspace with nothing indexed yet.
///
/// Deliberately empty: the point of the watcher tests is what happens to files
/// that appear *after* the app is running, which is the case a one-shot scan
/// can never cover.
fn watchable_workspace() -> (tempfile::TempDir, Core) {
    let corpus = tempfile::tempdir().expect("corpus dir");
    let db_dir = tempfile::tempdir().expect("db dir");
    // Leaked on purpose: the Core below outlives this function and the
    // database must not be removed under it. The corpus dir is returned.
    let db = Box::leak(Box::new(db_dir)).path().join("marrow.db");

    let store =
        Store::open_with_migrations(db.clone(), marrow_index::MIGRATIONS).expect("open store");
    let now = Timestamp::now();
    let ws = store
        .upsert_workspace(NewWorkspace {
            workspace_id: WorkspaceId::new(),
            name: "watched".into(),
            at: now,
        })
        .expect("workspace");
    let root = AuthorizedRoot::open(corpus.path()).expect("authorize root");
    store
        .upsert_root(NewRoot {
            root_id: RootId::new(),
            workspace_id: ws,
            canonical_path: root.path().to_string_lossy().into_owned(),
            volume_identity: None,
            grant_token: None,
            storage_kind: StorageKind::Local,
            cloud_provider: None,
            at: now,
        })
        .expect("root");
    store.flush().expect("flush");
    drop(store);

    (corpus, Core::open(db).expect("core"))
}

/// Build an index over a temporary corpus, and a hub pointed at the real
/// downloaded models.
fn indexed_corpus() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let corpus = tempfile::tempdir().expect("corpus dir");
    std::fs::write(corpus.path().join("lease.md"), LEASE).expect("write lease");
    std::fs::write(corpus.path().join("handbook.md"), HANDBOOK).expect("write handbook");

    let db_dir = tempfile::tempdir().expect("db dir");
    let db = db_dir.path().join("marrow.db");

    let store =
        Store::open_with_migrations(db.clone(), marrow_index::MIGRATIONS).expect("open store");
    let now = Timestamp::now();
    let ws = store
        .upsert_workspace(NewWorkspace {
            workspace_id: WorkspaceId::new(),
            name: "lease".into(),
            at: now,
        })
        .expect("workspace");
    let root = AuthorizedRoot::open(corpus.path()).expect("authorize root");
    let root_id = store
        .upsert_root(NewRoot {
            root_id: RootId::new(),
            workspace_id: ws,
            canonical_path: root.path().to_string_lossy().into_owned(),
            volume_identity: None,
            grant_token: None,
            storage_kind: StorageKind::Local,
            cloud_provider: None,
            at: now,
        })
        .expect("root");
    store.flush().expect("flush");

    let index = marrow_index::Fts5Index::open(&store).expect("index");
    let outcome = ingest_root_with_index(
        &store,
        ws,
        root_id,
        &root,
        &IngestPolicy::default(),
        &Arc::new(Progress::new()),
        &IngestCancel::new(),
        Some(&index),
    )
    .expect("ingest");
    assert!(outcome.stored >= 2, "expected both files: {outcome:?}");
    drop(index);
    drop(store);

    (corpus, db_dir, db)
}

#[test]
#[ignore = "needs a downloaded model and about ten seconds"]
fn a_question_about_real_files_is_answered_from_them_with_a_citation() {
    let (_corpus, _db_dir, db) = indexed_corpus();

    let core = Arc::new(Core::open(db).expect("core"));
    let hub = Arc::new(Hub::start(data_dir().join("models"), &[]));

    let snapshot = hub.snapshot();
    if !snapshot.runtime_ready {
        panic!("no MLX runtime; see the Models page for the two commands");
    }
    let Some(model) = hub.generator() else {
        panic!("no model installed. Download one first, or run:\n  cargo test -p marrow-model -- --ignored the_smallest_pinned_model");
    };
    eprintln!("answering with {model}\n");

    let mut answer = String::new();
    let mut sources = Vec::new();
    let mut failure = None;
    let started = std::time::Instant::now();

    ask::run(
        &core,
        &hub,
        "conversation-1",
        "When does the lease renew and what is the rent?",
        &[],
        false,
        &Cancel::new(),
        &mut |e| match e {
            ask::AskEvent::Token { text } => answer.push_str(&text),
            ask::AskEvent::Sources { hits, .. } => {
                sources = hits.iter().map(|h| h.relative_path.clone()).collect();
            }
            ask::AskEvent::Failed { code, message } => {
                failure = Some(format!("[{code}] {message}"));
            }
            _ => {}
        },
    );

    if let Some(f) = failure {
        panic!("the pipeline failed: {f}");
    }
    eprintln!(
        "--- {} in {:.1}s ---\nsources: {sources:?}\n{}\n",
        model,
        started.elapsed().as_secs_f32(),
        answer.trim()
    );

    assert!(!answer.trim().is_empty(), "the model said nothing");
    assert!(
        sources.iter().any(|s| s.contains("lease")),
        "the lease should have been retrieved: {sources:?}"
    );
    // The two facts that exist only in the file. A plausible answer containing
    // neither means the model recalled a lease rather than reading this one,
    // which is the failure that destroys trust in the whole product.
    assert!(
        answer.contains("2031"),
        "the renewal date must come from the file: {answer}"
    );
    assert!(
        answer.contains("2,417") || answer.contains("2417"),
        "the rent must come from the file: {answer}"
    );
}

/// Where do two turns' prompts stop matching? Diagnostic, and it needs no
/// model — the envelope is assembled without one.
#[test]
fn the_second_turns_prompt_shares_its_preamble_with_the_first() {
    let (_corpus, _db_dir, db) = indexed_corpus();
    let core = Core::open(db).expect("core");
    let mut convo = marrow_desktop::models::Conversation::default();

    let (first, _, _) = ask::assemble(&core, "When does the lease renew?", &[], &mut convo, None)
        .expect("assemble one");
    let turns = ask::turns_from(&[
        ask::PriorTurn {
            role: "user".into(),
            text: "When does the lease renew?".into(),
        },
        ask::PriorTurn {
            role: "assistant".into(),
            text: "31 December 2031 [E1].".into(),
        },
    ]);
    let (second, _, _) = ask::assemble(&core, "And what is the rent?", &turns, &mut convo, None)
        .expect("assemble two");

    let shared = first
        .text
        .as_bytes()
        .iter()
        .zip(second.text.as_bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let fraction = shared as f64 / first.text.len() as f64;
    eprintln!(
        "shared {shared} of {} bytes ({:.0}%)\n--- first diverges at ---\n{}\n",
        first.text.len(),
        fraction * 100.0,
        &first.text[shared.saturating_sub(120)..(shared + 200).min(first.text.len())]
    );
    assert!(
        fraction > 0.5,
        "two turns about the same document must share most of their prompt"
    );
}

#[test]
#[ignore = "needs a downloaded model"]
fn a_follow_up_reuses_the_prompt_and_keeps_the_thread() {
    // The reason Ask is a conversation rather than a box: the second question
    // is about the first, and everything above it in the prompt is identical.
    let (_corpus, _db_dir, db) = indexed_corpus();
    let core = Arc::new(Core::open(db).expect("core"));
    let hub = Arc::new(Hub::start(data_dir().join("models"), &[]));
    if hub.generator().is_none() {
        panic!("no model installed");
    }

    let mut first = String::new();
    let mut usage_two = None;
    let convo = "conversation-2";

    ask::run(
        &core,
        &hub,
        convo,
        "When does the lease renew?",
        &[],
        false,
        &Cancel::new(),
        &mut |e| {
            if let ask::AskEvent::Token { text } = e {
                first.push_str(&text);
            }
        },
    );
    assert!(!first.trim().is_empty(), "the first answer was empty");

    let history = vec![
        ask::PriorTurn {
            role: "user".into(),
            text: "When does the lease renew?".into(),
        },
        ask::PriorTurn {
            role: "assistant".into(),
            text: first.clone(),
        },
    ];
    let turns = ask::turns_from(&history);

    let mut second = String::new();
    ask::run(
        &core,
        &hub,
        convo,
        "And what is the rent?",
        &turns,
        false,
        &Cancel::new(),
        &mut |e| match e {
            ask::AskEvent::Token { text } => second.push_str(&text),
            ask::AskEvent::Done {
                prompt_tokens,
                cached_prefix_tokens,
                ..
            } => usage_two = Some((prompt_tokens, cached_prefix_tokens)),
            _ => {}
        },
    );

    let (prompt, cached) = usage_two.expect("the second turn must report usage");
    eprintln!(
        "\n--- follow-up: {cached} of {prompt} prompt tokens reused ---\n{}\n",
        second.trim()
    );
    assert!(
        second.contains("2,417") || second.contains("2417"),
        "the follow-up must still be grounded: {second}"
    );
    // Reuse is *not* asserted, and that is a finding rather than a gap in the
    // test. Whether a follow-up can reuse its preamble depends on whether the
    // model's cache can be trimmed, which is a property of the architecture:
    // Qwen 3 0.6B is pure `KVCache` and reuses about 80%, while Qwen 3.5 4B
    // mixes in `ArraysCache` and cannot be trimmed at all. The worker reports
    // which, so the difference is explained rather than merely felt.
    //
    // What *is* asserted is the part this code controls: the evidence is
    // carried forward, so the preamble is identical across turns and the reuse
    // is there for any model able to take it.
    let _ = cached;
    assert!(
        prompt > 0,
        "the second turn must have accounted for its prompt"
    );
}

#[test]
#[ignore = "needs the embedding model and about thirty seconds"]
fn semantic_retrieval_finds_a_document_that_shares_no_words_with_the_question() {
    // The whole point of M4, and the thing lexical search cannot do. The
    // question says "renew"; the document says "continues automatically at the
    // conclusion of each successive term". No shared content word.
    let (corpus, _db_dir, db) = indexed_corpus();
    std::fs::write(corpus.path().join("paraphrase.md"), PARAPHRASE).unwrap();

    let core = Arc::new(Core::open(db).expect("core"));
    let hub = Arc::new(Hub::start(data_dir().join("models"), &[]));

    // Re-index so the new file is in.
    reindex(&core, corpus.path());

    let Some(embedder) = hub.embedder() else {
        panic!(
            "no embedder: {}",
            hub.embedder_problem().unwrap_or_else(|| "unknown".into())
        );
    };
    let progress = Arc::new(marrow_model::backfill::Progress::default());
    let out = marrow_model::backfill::run(
        core.store(),
        core.vectors(),
        &embedder,
        &Cancel::new(),
        &progress,
    )
    .expect("backfill");
    eprintln!("embedded {} chunks", out.embedded);
    assert!(out.embedded > 0, "nothing was embedded");

    let question = "when does the lease renew?";

    // Lexical alone: the paraphrase shares no content word with the question,
    // so it cannot be found this way.
    let lexical = core.retrieve(question, 10, None).expect("lexical");
    eprintln!(
        "lexical: {:?}",
        lexical
            .iter()
            .map(|c| c.relative_path.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        !lexical
            .iter()
            .any(|c| c.relative_path.contains("paraphrase")),
        "the paraphrase should be unreachable lexically, or this proves nothing"
    );

    // With the semantic branch, it is.
    let embedding = embedder.embed_one(question).expect("embed the question");
    let hybrid = core
        .retrieve(question, 10, Some(&embedding))
        .expect("hybrid");
    eprintln!(
        "hybrid: {:?}",
        hybrid
            .iter()
            .map(|c| c.relative_path.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        hybrid
            .iter()
            .any(|c| c.relative_path.contains("paraphrase")),
        "semantic search did not find the paraphrase"
    );
}

/// Index a corpus into an already-open core.
fn reindex(core: &Core, corpus: &std::path::Path) {
    let root = AuthorizedRoot::open(corpus).unwrap();
    let conn = core.store().reader().unwrap();
    let (ws, root_id): (String, String) = conn
        .query_row(
            "SELECT workspace_id, root_id FROM workspace_roots LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    drop(conn);
    let index = marrow_index::Fts5Index::open(core.store()).unwrap();
    ingest_root_with_index(
        core.store(),
        ws.parse().unwrap(),
        root_id.parse().unwrap(),
        &root,
        &IngestPolicy::default(),
        &Arc::new(Progress::new()),
        &IngestCancel::new(),
        Some(&index),
    )
    .unwrap();
    core.store().flush().unwrap();
}

#[test]
#[ignore = "reproduces the reported truncation; needs a model"]
fn a_long_answer_is_not_silently_cut_off() {
    // Reported from real use: "ask stops in between answering". Reproduced
    // here by asking for something that genuinely needs many tokens, and
    // checking what the pipeline says about why it stopped.
    let (_corpus, _db_dir, db) = indexed_corpus();
    let core = Arc::new(Core::open(db).expect("core"));
    let hub = Arc::new(Hub::start(data_dir().join("models"), &[]));
    if hub.generator().is_none() {
        panic!("no model installed");
    }

    let mut answer = String::new();
    let mut stop = String::new();
    let mut out_tokens = 0u32;
    let mut failure = None;

    ask::run(
        &core,
        &hub,
        "truncation",
        "Summarise every clause of the lease in detail, one numbered paragraph \
         each, and then explain what each one means for the tenant.",
        &[],
        false,
        &Cancel::new(),
        &mut |e| match e {
            ask::AskEvent::Token { text } => answer.push_str(&text),
            ask::AskEvent::Done {
                stop_reason,
                output_tokens,
                ..
            } => {
                stop = stop_reason;
                out_tokens = output_tokens;
            }
            ask::AskEvent::Failed { code, message } => {
                failure = Some(format!("[{code}] {message}"))
            }
            _ => {}
        },
    );

    if let Some(f) = failure {
        panic!("the pipeline failed rather than truncating: {f}");
    }
    eprintln!(
        "\n--- {out_tokens} tokens, stop = {stop} ---\n{}\n[...]\n{}\n",
        answer.chars().take(200).collect::<String>(),
        answer
            .chars()
            .rev()
            .take(200)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    );

    // The bug was not that a limit exists — it was a flat 1,024 low enough to
    // cut an ordinary answer. The budget is derived from the window now, so an
    // ordinary question must finish; and when a limit *is* reached, the UI says
    // so where the answer stops rather than only in the footer.
    assert_ne!(
        stop, "length",
        "an ordinary answer still hit the ceiling; {out_tokens} tokens"
    );
    assert!(
        out_tokens > 200,
        "this question should produce a real answer"
    );
}

/// The watcher, which is the difference between an index and a snapshot.
///
/// A stale index is worse than no index: no index answers nothing and the user
/// knows to scan, a stale one answers confidently about a disk it has not
/// looked at. These pin that the app closes that gap itself rather than
/// depending on someone running `marrow index` in a terminal.
mod watching {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use marrow_desktop::Watchers;

    /// Poll until `f` holds or the deadline passes. Filesystem events are
    /// asynchronous by nature; a fixed sleep is either flaky or slow.
    fn within(secs: u64, mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        f()
    }

    #[test]
    fn a_file_created_while_the_app_is_open_becomes_searchable_without_a_manual_scan() {
        let (dir, core) = super::watchable_workspace();
        let core = Arc::new(core);
        let watchers = Watchers::start(Arc::clone(&core)).expect("watchers start");

        std::fs::write(
            dir.path().join("note.md"),
            "the quarterly renewal clause is unusual\n",
        )
        .unwrap();

        let found = within(20, || {
            core.search("renewal", 5)
                .map(|r| !r.hits.is_empty())
                .unwrap_or(false)
        });
        watchers.stop();
        assert!(
            found,
            "a file written while the app was open never reached the index"
        );
    }

    /// **The ordinary case, not the edge one.** You work on files all day with
    /// Marrow closed, then open it. Everything that changed in between produced
    /// no event for anyone to hear, so without a sweep at startup the app shows
    /// an index from whenever a scan last ran — and waits six hours to notice.
    /// That is indistinguishable from having no watcher at all.
    #[test]
    fn changes_made_while_the_app_was_closed_are_picked_up_when_it_opens() {
        let (dir, core) = super::watchable_workspace();
        let core = Arc::new(core);

        // Written before anything is watching: no event exists to be missed.
        std::fs::write(
            dir.path().join("closed.md"),
            "the sublet provision was added last night\n",
        )
        .unwrap();
        assert!(
            core.search("sublet", 5).unwrap().hits.is_empty(),
            "nothing has scanned yet, so this must not be findable"
        );

        let watchers = Watchers::start(Arc::clone(&core)).expect("watchers start");
        let found = within(20, || {
            core.search("sublet", 5)
                .map(|r| !r.hits.is_empty())
                .unwrap_or(false)
        });
        watchers.stop();
        assert!(
            found,
            "opening the app did not reconcile what changed while it was shut"
        );
    }

    /// The freshness the *other* processes read. It lives in the database
    /// rather than this app's memory precisely so the MCP server and the CLI —
    /// separate, short-lived processes — can tell how current the index is.
    #[test]
    fn watching_is_recorded_where_another_process_can_read_it() {
        let (_dir, core) = super::watchable_workspace();
        let core = Arc::new(core);

        let before = marrow_query::catalog::index_stats(&core.store().reader().unwrap()).unwrap();
        assert!(
            before.may_be_stale(),
            "a database nobody has watched must not report itself fresh"
        );

        let watchers = Watchers::start(Arc::clone(&core)).expect("watchers start");
        let fresh = within(20, || {
            marrow_query::catalog::index_stats(&core.store().reader().unwrap())
                .map(|s| !s.may_be_stale())
                .unwrap_or(false)
        });
        watchers.stop();
        assert!(fresh, "a running watcher was never recorded in the store");

        // And stopping puts it back: an app that has quit must not leave a
        // database claiming someone is still watching it.
        let after = marrow_query::catalog::index_stats(&core.store().reader().unwrap()).unwrap();
        assert!(
            after.may_be_stale(),
            "after the watchers stopped the index still reported itself watched"
        );
    }
}
