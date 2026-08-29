//! Canonical SQLite schema — M1 subset of [Part 6 §106].
//!
//! Only the eleven tables the ROADMAP stages into M1 are here:
//! `schema_meta`, `devices`, `workspaces`, `workspace_roots`, `files`,
//! `file_paths`, `file_versions`, `parse_results`, `ir_nodes`, `chunks`,
//! `jobs`. Carrying the other ~30 tables before anything writes to them is
//! dead weight (ROADMAP "Schema staging").
//!
//! Every column §106 defines for those tables is kept, including ones nothing
//! reads yet — they are the retrofit-expensive part. What is *dropped* is
//! foreign keys pointing at tables that do not exist in M1:
//!
//! | Dropped FK | Points at | Kept as |
//! |---|---|---|
//! | `workspaces.principal_id` | `principals` (§106.2, MULTI) | nullable `TEXT` |
//! | `chunks.table_id` | `table_ir` (§106.6, M3) | plain `TEXT` |
//!
//! Three indexes are *added* beyond §106, all of them enforcing or serving a
//! query the M1 store actually issues. They are noted at their definitions.

use marrow_core::{Code, Error, Result};
use rusqlite::Connection;

/// Pragmas for the write connection (§106.1, Part 2 §50).
///
/// `mmap_size` from §106.1 is deliberately not set: it is a throughput knob and
/// M0 measured 235k rows/s without it, so it buys nothing and costs address
/// space. Revisit only with a benchmark (ROADMAP scope rule 6).
const WRITER_PRAGMAS: &str = "\
PRAGMA synchronous  = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
PRAGMA temp_store   = MEMORY;
";

/// Pragmas for read connections.
///
/// `journal_mode` is a property of the database file, not the connection, so
/// readers must not try to set it. `query_only` is the machine-checkable form
/// of "never open a second write connection" (Part 2 §50).
const READER_PRAGMAS: &str = "\
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
PRAGMA temp_store   = MEMORY;
PRAGMA query_only   = ON;
";

/// The M1 tables, in dependency order. Used by tests and diagnostics.
pub const M1_TABLES: &[&str] = &[
    "schema_meta",
    "devices",
    "workspaces",
    "workspace_roots",
    "files",
    "file_paths",
    "file_versions",
    "parse_results",
    "ir_nodes",
    "chunks",
    "jobs",
];

/// Migration 1: the whole M1 subset.
pub const SCHEMA_V1: &str = r#"
-- ---------------------------------------------------------------- §106.2 meta
CREATE TABLE schema_meta (
    key                 TEXT PRIMARY KEY,
    value               TEXT NOT NULL
);  -- schema_version, created_at, app_version_at_create

CREATE TABLE devices (
    device_id           TEXT PRIMARY KEY,
    platform            TEXT NOT NULL,
    first_seen_at       INTEGER NOT NULL,
    last_seen_at        INTEGER NOT NULL
);

-- ---------------------------------------------------- §106.3 workspaces/roots
CREATE TABLE workspaces (
    workspace_id        TEXT PRIMARY KEY,
    -- §106.2 `principals` is a MULTI (multi-OS-account) table and is not in M1.
    -- Column kept so the FK can be added back by a later migration; nullable
    -- because M1 has no principal to point at.
    principal_id        TEXT,
    name                TEXT NOT NULL,
    data_classification TEXT NOT NULL DEFAULT 'INTERNAL' CHECK (data_classification IN
                          ('PUBLIC','INTERNAL','CONFIDENTIAL','RESTRICTED','LOCAL_ONLY')),
    include_policy      TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(include_policy)),
    exclude_policy      TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(exclude_policy)),
    model_policy        TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(model_policy)),
    action_policy       TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(action_policy)),
    retention_policy    TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(retention_policy)),
    indexing_policy     TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(indexing_policy)),
    media_policy        TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(media_policy)),
    exec_policy         TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(exec_policy)),
    locked              INTEGER NOT NULL DEFAULT 0,
    status              TEXT NOT NULL DEFAULT 'ACTIVE'
                          CHECK (status IN ('ACTIVE','PAUSED','UNAVAILABLE','FORGETTING','REMOVED')),
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    -- SYNC-006: which device authored this row. Nullable and unused on a
    -- single device; present from day one because adding it later means a
    -- migration across every canonical table plus a backfill that cannot know
    -- the answer. `origin_principal_id` is deliberately NOT here -- Part 7
    -- §124 reduced MULTI to three requirements and dropped per-principal
    -- tracking, so a `principals` table never arrives.
    origin_device_id    TEXT REFERENCES devices(device_id)
);
-- ADDED beyond §106: workspace names are the natural key the CLI and the
-- upsert path use ("workspace add ~/Desktop"). Two workspaces with one name is
-- a bug, so the database says so.
CREATE UNIQUE INDEX idx_workspaces_name ON workspaces(name);

CREATE TABLE workspace_roots (
    root_id             TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    canonical_path      TEXT NOT NULL,
    volume_identity     TEXT,
    grant_token         TEXT,
    storage_kind        TEXT NOT NULL DEFAULT 'LOCAL' CHECK (storage_kind IN
                          ('LOCAL','REMOVABLE','NETWORK','TIERED_CLOUD','REDIRECTED_PROFILE')),
    cloud_provider      TEXT,
    watcher_health      TEXT NOT NULL DEFAULT 'LIVE'
                          CHECK (watcher_health IN ('LIVE','DEGRADED','POLL_ONLY','UNAVAILABLE')),
    watch_cursor        TEXT,
    last_reconciled_at  INTEGER,
    reconcile_interval_ms INTEGER NOT NULL DEFAULT 21600000,
    availability        TEXT NOT NULL DEFAULT 'AVAILABLE',
    created_at          INTEGER NOT NULL,
    origin_device_id    TEXT REFERENCES devices(device_id)  -- SYNC-006
);
CREATE INDEX idx_roots_workspace ON workspace_roots(workspace_id);
-- ADDED beyond §106: the same canonical path granted twice inside one
-- workspace would index every file twice under two root_ids. Also the conflict
-- target for the root upsert.
CREATE UNIQUE INDEX idx_roots_ws_path ON workspace_roots(workspace_id, canonical_path);

-- ------------------------------------------------ §106.4 files, paths, versions
CREATE TABLE files (
    file_id             TEXT PRIMARY KEY,            -- stable logical identity (FS-005)
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    root_id             TEXT NOT NULL REFERENCES workspace_roots(root_id),
    current_path        TEXT,                        -- NULL when deleted
    fs_identity         TEXT,                        -- inode+dev / FileId; may be NULL
    -- No FK: file_versions.file_id points back here, and SQLite cannot satisfy
    -- a mutual FK pair in one INSERT without deferred constraints. §106 leaves
    -- this untyped for the same reason.
    current_version_id  TEXT,
    tier_state          TEXT NOT NULL DEFAULT 'RESIDENT' CHECK (tier_state IN
                          ('RESIDENT','PLACEHOLDER','HYDRATING','UNAVAILABLE')),  -- TIER-001
    origin              TEXT NOT NULL DEFAULT 'USER' CHECK (origin IN ('USER','SELF')),
    origin_txn_id       TEXT,                        -- set when origin = SELF
    external_source_url TEXT,                        -- META-004 download origin
    status              TEXT NOT NULL DEFAULT 'ACTIVE'
                          CHECK (status IN ('ACTIVE','DELETED','EXCLUDED','ERROR','FORGOTTEN')),
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    origin_device_id    TEXT REFERENCES devices(device_id)  -- SYNC-006
);
CREATE INDEX idx_files_ws_status   ON files(workspace_id, status);
CREATE INDEX idx_files_path        ON files(workspace_id, current_path);
CREATE INDEX idx_files_fs_identity ON files(root_id, fs_identity);
CREATE INDEX idx_files_tier        ON files(workspace_id, tier_state) WHERE tier_state != 'RESIDENT';
-- ADDED beyond §106: the scanner's hot lookup is by (root_id, path) — it walks
-- one root at a time and does not know the workspace_id at that point.
CREATE INDEX idx_files_root_path   ON files(root_id, current_path);

CREATE TABLE file_paths (               -- FS-006 path history
    path_id             TEXT PRIMARY KEY,
    file_id             TEXT NOT NULL REFERENCES files(file_id) ON DELETE CASCADE,
    path                TEXT NOT NULL,
    observed_from       INTEGER NOT NULL,
    observed_to         INTEGER
);
CREATE INDEX idx_file_paths_file ON file_paths(file_id);
CREATE INDEX idx_file_paths_path ON file_paths(path);

CREATE TABLE file_versions (
    version_id          TEXT PRIMARY KEY,
    file_id             TEXT NOT NULL REFERENCES files(file_id) ON DELETE CASCADE,
    path_at_observation TEXT NOT NULL,
    size_bytes          INTEGER NOT NULL,
    mtime_ms            INTEGER NOT NULL,
    content_hash        TEXT NOT NULL,               -- BLAKE3
    mime                TEXT,                        -- probed, not from extension (FS-014)
    language            TEXT,                        -- I18N-001
    observed_at         INTEGER NOT NULL,
    supersedes          TEXT REFERENCES file_versions(version_id),
    status              TEXT NOT NULL DEFAULT 'CURRENT'
                          CHECK (status IN ('CURRENT','HISTORICAL','TOMBSTONED')),
    origin_device_id    TEXT REFERENCES devices(device_id)  -- SYNC-006
);
CREATE INDEX idx_versions_file ON file_versions(file_id, status);
-- Invariant: exactly one CURRENT version per file (§106.12).
CREATE UNIQUE INDEX idx_versions_current ON file_versions(file_id) WHERE status = 'CURRENT';
CREATE INDEX idx_versions_hash ON file_versions(content_hash);   -- FS-008 dedup

-- --------------------------------------------------------- §106.5 parsing / IR
CREATE TABLE parse_results (
    parse_id            TEXT PRIMARY KEY,
    version_id          TEXT NOT NULL REFERENCES file_versions(version_id) ON DELETE CASCADE,
    parser_id           TEXT NOT NULL,
    parser_version      TEXT NOT NULL,               -- PAR-003
    parser_tier         TEXT NOT NULL CHECK (parser_tier IN ('T1','T2','T3','T4','T5')),
    provenance_class    TEXT NOT NULL CHECK (provenance_class IN
                          ('EXACT','DEGRADED','APPROXIMATE','METADATA_ONLY')),  -- CONV-003
    -- METADATA_ONLY is the router's terminal outcome (Part 3 §63 T5): a file
    -- with no parser stays discoverable from metadata, which is a fact about
    -- the file, not an error. Part 6 §106.5's CHECK omits it, so persisting a
    -- parse result for any binary would have failed the constraint — on this
    -- corpus that is every one of the 3,478 photos.
    outcome             TEXT NOT NULL CHECK (outcome IN
                          ('OK','PARTIAL','LOW_YIELD','FAILED','UNSUPPORTED',
                           'SKIPPED_POLICY','METADATA_ONLY')),
    char_yield          INTEGER,
    page_count          INTEGER,
    warnings            TEXT CHECK (warnings IS NULL OR json_valid(warnings)),
    parsed_at           INTEGER NOT NULL,
    UNIQUE(version_id, parser_id, parser_version)    -- §20.2 idempotency key
);

CREATE TABLE ir_nodes (
    node_id             TEXT PRIMARY KEY,
    version_id          TEXT NOT NULL REFERENCES file_versions(version_id) ON DELETE CASCADE,
    parent_node_id      TEXT REFERENCES ir_nodes(node_id),
    kind                TEXT NOT NULL,               -- §8.6 IrNodeKind
    ordinal             INTEGER NOT NULL,
    -- Invariant #1: provenance is NOT NULL by construction. A node without a
    -- source_span is a bug, so the schema refuses to store one.
    source_span         TEXT NOT NULL CHECK (json_valid(source_span)),
    attributes          TEXT CHECK (attributes IS NULL OR json_valid(attributes)),
    text_hash           TEXT,                        -- CHK-007 IR diffing
    trust               TEXT NOT NULL DEFAULT 'UNTRUSTED_CONTENT' CHECK (trust IN
                          ('DETERMINISTIC_RUNTIME','UNTRUSTED_CONTENT'))   -- PAR-014
);
CREATE INDEX idx_ir_version ON ir_nodes(version_id, ordinal);
CREATE INDEX idx_ir_parent  ON ir_nodes(parent_node_id);
CREATE INDEX idx_ir_kind    ON ir_nodes(version_id, kind);

-- -------------------------------------------------------------- §106.7 chunks
CREATE TABLE chunks (
    chunk_id            TEXT PRIMARY KEY,
    version_id          TEXT NOT NULL REFERENCES file_versions(version_id) ON DELETE CASCADE,
    root_node_id        TEXT REFERENCES ir_nodes(node_id),
    -- FK to table_ir dropped: that table arrives in M3. Column kept.
    table_id            TEXT,
    chunk_kind          TEXT NOT NULL DEFAULT 'TEXT' CHECK (chunk_kind IN
                          ('TEXT','CODE','TABLE_BAND','TABLE_SCHEMA','TRANSCRIPT',
                           'IMAGE_DESCRIPTION','OCR_TEXT','METADATA')),
    text                TEXT NOT NULL,
    context_prefix      TEXT,                        -- CHK-002 parent headings
    token_count         INTEGER NOT NULL,
    text_hash           TEXT NOT NULL,               -- embedding cache key (EMB-008)
    chunker_version     TEXT NOT NULL,
    provenance_class    TEXT NOT NULL DEFAULT 'EXACT',
    extraction_method   TEXT NOT NULL DEFAULT 'NATIVE',   -- NATIVE | OCR | ASR | VLM | T3
    status              TEXT NOT NULL DEFAULT 'ACTIVE'
                          CHECK (status IN ('ACTIVE','SUPERSEDED','TOMBSTONED'))
);
CREATE INDEX idx_chunks_version ON chunks(version_id, status);
CREATE INDEX idx_chunks_hash    ON chunks(text_hash);

-- ---------------------------------------------------------------- §106.10 jobs
CREATE TABLE jobs (
    job_id              TEXT PRIMARY KEY,
    workspace_id        TEXT REFERENCES workspaces(workspace_id),
    job_type            TEXT NOT NULL,               -- §20.1
    target_id           TEXT,
    target_version      TEXT,
    idempotency_key     TEXT NOT NULL,               -- §20.2
    priority            INTEGER NOT NULL DEFAULT 3,  -- §21.2 P0..P5
    attempt             INTEGER NOT NULL DEFAULT 0,
    max_attempts        INTEGER NOT NULL DEFAULT 3,
    status              TEXT NOT NULL DEFAULT 'PENDING' CHECK (status IN
                          ('PENDING','LEASED','RUNNING','DONE','FAILED','DEAD','CANCELLED')),
    lease_owner         TEXT,
    lease_expires_at    INTEGER,
    not_before          INTEGER NOT NULL DEFAULT 0,  -- backoff
    last_error_code     TEXT,
    last_error_detail   TEXT,
    payload             TEXT CHECK (payload IS NULL OR json_valid(payload)),
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    UNIQUE(idempotency_key)                          -- §111 re-enqueue is a no-op
);
CREATE INDEX idx_jobs_queue ON jobs(status, priority, not_before)
    WHERE status IN ('PENDING','LEASED');
CREATE INDEX idx_jobs_ws    ON jobs(workspace_id, status);
"#;

/// Apply the write-connection pragmas, including WAL.
pub fn apply_writer_pragmas(conn: &Connection) -> Result<()> {
    // `PRAGMA journal_mode` returns a row, so it cannot go through execute_batch
    // reliably — and its answer matters: a database that refuses WAL would give
    // us a single-reader store without saying so.
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .map_err(|e| {
            crate::map_sqlite(
                e,
                "Could not put the index database into WAL mode. The file may be on a \
                 filesystem that does not support it (some network shares); move the index \
                 to local disk.",
            )
        })?;
    // "memory" is what an in-memory database reports; it cannot do WAL and does
    // not need to. Anything else is a real refusal.
    if !matches!(mode.to_ascii_lowercase().as_str(), "wal" | "memory") {
        return Err(Error::new(
            Code::DbCorrupt,
            "The index database refused WAL mode, so concurrent readers would block writes. \
             Move the index to local disk, or delete it to rebuild.",
        )
        .with_context(format!("journal_mode = {mode}")));
    }
    conn.execute_batch(WRITER_PRAGMAS)
        .map_err(|e| crate::map_sqlite(e, "Could not configure the index database connection."))?;
    Ok(())
}

/// Apply the read-connection pragmas. Readers are `query_only`.
pub fn apply_reader_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(READER_PRAGMAS).map_err(|e| {
        crate::map_sqlite(
            e,
            "Could not configure a read connection to the index database.",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        apply_writer_pragmas(&conn).unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn
    }

    #[test]
    fn ddl_creates_exactly_the_m1_table_set() {
        let conn = schema_conn();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        let mut found: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        found.sort();
        let mut want: Vec<String> = M1_TABLES.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(found, want, "M1 table set drifted from ROADMAP staging");
    }

    #[test]
    fn no_graph_action_or_media_tables_leaked_in() {
        let conn = schema_conn();
        for forbidden in [
            "entities",
            "relations",
            "facts",
            "evidence",
            "action_transactions",
            "media_derivatives",
            "table_ir",
            "chunk_vectors",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                    [forbidden],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "{forbidden} is not an M1 table");
        }
    }

    #[test]
    fn foreign_keys_are_on_and_enforced() {
        let conn = schema_conn();
        let on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(on, 1);
        // A root pointing at a workspace that does not exist must be refused.
        let err = conn.execute(
            "INSERT INTO workspace_roots (root_id, workspace_id, canonical_path, created_at)
             VALUES ('r', 'nope', '/tmp', 0)",
            [],
        );
        assert!(err.is_err(), "dangling FK must be rejected");
    }

    #[test]
    fn every_declared_foreign_key_points_at_a_table_that_exists() {
        // `PRAGMA foreign_key_check` is empty on an empty database even with a
        // dangling *table* reference, so ask the parser directly.
        let conn = schema_conn();
        for table in M1_TABLES {
            let mut stmt = conn
                .prepare(&format!("PRAGMA foreign_key_list({table})"))
                .unwrap();
            let targets: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(2))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            for t in targets {
                assert!(
                    M1_TABLES.contains(&t.as_str()),
                    "{table} has an FK to {t}, which is not an M1 table"
                );
            }
        }
    }

    #[test]
    fn pragmas_match_the_spec() {
        let conn = schema_conn();
        let busy: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(busy, 5000);
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1, "synchronous = NORMAL");
        let temp: i64 = conn
            .query_row("PRAGMA temp_store", [], |r| r.get(0))
            .unwrap();
        assert_eq!(temp, 2, "temp_store = MEMORY");
    }
}
