# Marrow — Master Specification, Part 6

## Engineering Reference: Schema, Contracts, Algorithms, Test and Release

**Status:** Addendum to Marrow Master Specification Parts 1–5
**Date:** 30 August 2026
**Numbering:** Continues from §104 of Part 5
**Format:** Reference material — DDL, contracts, algorithms, tables

---

# 105. Scope

Parts 1–5 specify *what* the system must do and *why*. They sketch the data model (§9), the graph tables (§8.12), the IPC envelope (§7.2) and the API boundaries (§30) without ever making them concrete enough to build from.

This part is the implementable reference. Where it conflicts with an earlier sketch, **this part supersedes it**, because the earlier text was illustrative.

| § | Artifact | Supersedes |
|---|---|---|
| 106 | Canonical SQLite schema (DDL) | §9, §8.12 sketches |
| 107 | Migration and versioning mechanics | NFR-011, PKG-010 |
| 108 | Error taxonomy | — new |
| 109 | Configuration reference | §19 sketch |
| 110 | IPC contract (CQRS registry) | §7.2, §30 |
| 111 | Job system detail | §20 |
| 112 | Chunking algorithm | CHK block |
| 113 | Retrieval fusion baseline | §12.2 |
| 114 | Context envelope serialization | §12.3 |
| 115 | Prompt and template governance | — new |
| 116 | Test strategy and CI | §29, §56 |
| 117 | Build and release engineering | PKG block |
| 118 | Frontend architecture | §16, §17.1 |
| 119 | Consolidated performance budgets | NFR block |
| 120 | Glossary | — new |
| 121 | Consolidated indexes | §57, §59, §55 |
| 122 | Documentation map | — new |

---

# 106. Canonical SQLite schema

## 106.1 Conventions

| Convention | Rule |
|---|---|
| Primary keys | `TEXT` ULID (SYNC-005). Lexicographically sortable by creation time; globally unique for future merge |
| Timestamps | `INTEGER` epoch milliseconds, **UTC always**. Never local time, never text |
| Booleans | `INTEGER` 0/1 |
| JSON | `TEXT` with a `json_valid()` CHECK; use only where the shape is genuinely open |
| Deletion | Soft, via `status`. Physical deletion only through the §26 forget path |
| Provenance | `origin_device_id` + `origin_principal_id` on every user- or agent-mutable row (SYNC-006, MULTI extension) |
| Derived data | Everything rebuildable from canonical state carries `(source_id, processor_id, processor_version)` for idempotency (§20.2) |
| Enums | `TEXT` with a CHECK constraint, not integers — readable in a debugger, and migration-safe |
| Foreign keys | `ON` (§50). `ON DELETE RESTRICT` by default; cascades only where the child is meaningless alone |

```sql
PRAGMA journal_mode   = WAL;
PRAGMA synchronous    = NORMAL;
PRAGMA foreign_keys   = ON;
PRAGMA busy_timeout   = 5000;
PRAGMA temp_store     = MEMORY;
PRAGMA mmap_size      = 268435456;
```

## 106.2 Meta and identity

```sql
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

CREATE TABLE principals (               -- MULTI: OS account scoping
    principal_id        TEXT PRIMARY KEY,
    device_id           TEXT NOT NULL REFERENCES devices(device_id),
    os_identity_hash    TEXT NOT NULL,  -- salted hash of UID/SID; never the raw value
    created_at          INTEGER NOT NULL,
    UNIQUE(device_id, os_identity_hash)
);

CREATE TABLE hardware_profiles (        -- HW-001
    profile_id          TEXT PRIMARY KEY,
    device_id           TEXT NOT NULL REFERENCES devices(device_id),
    probe_version       INTEGER NOT NULL,
    probed_at           INTEGER NOT NULL,
    capability_tier     TEXT NOT NULL CHECK (capability_tier IN
                          ('T_MIN','T_LOW','T_MID','T_HIGH','T_MAX')),
    cpu_cores           INTEGER, cpu_features TEXT,
    ram_total_mb        INTEGER, ram_available_mb INTEGER,
    gpu_vendor          TEXT, gpu_vram_mb INTEGER, unified_memory INTEGER NOT NULL DEFAULT 0,
    npu_available       INTEGER NOT NULL DEFAULT 0,
    accel_eps_verified  TEXT,           -- JSON: EPs that actually loaded (HW-003)
    disk_free_mb        INTEGER,
    detected_runtimes   TEXT            -- JSON: ollama/lmstudio/llama.cpp (LLM R1)
);
```

## 106.3 Workspaces and roots

```sql
CREATE TABLE workspaces (
    workspace_id        TEXT PRIMARY KEY,
    principal_id        TEXT NOT NULL REFERENCES principals(principal_id),
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
    locked              INTEGER NOT NULL DEFAULT 0,   -- MULTI-009 workspace lock
    status              TEXT NOT NULL DEFAULT 'ACTIVE'
                          CHECK (status IN ('ACTIVE','PAUSED','UNAVAILABLE','FORGETTING','REMOVED')),
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);

CREATE TABLE workspace_roots (
    root_id             TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    canonical_path      TEXT NOT NULL,
    volume_identity     TEXT,                        -- WS-009 removable volumes
    grant_token         TEXT,                        -- macOS security-scoped bookmark (§6.4)
    storage_kind        TEXT NOT NULL DEFAULT 'LOCAL' CHECK (storage_kind IN
                          ('LOCAL','REMOVABLE','NETWORK','TIERED_CLOUD','REDIRECTED_PROFILE')),
    cloud_provider      TEXT,                        -- TIER-012
    watcher_health      TEXT NOT NULL DEFAULT 'LIVE'
                          CHECK (watcher_health IN ('LIVE','DEGRADED','POLL_ONLY','UNAVAILABLE')),
    watch_cursor        TEXT,                        -- WATCH-006 FSEvents id / USN
    last_reconciled_at  INTEGER,
    reconcile_interval_ms INTEGER NOT NULL DEFAULT 21600000,
    availability        TEXT NOT NULL DEFAULT 'AVAILABLE',
    created_at          INTEGER NOT NULL
);
CREATE INDEX idx_roots_workspace ON workspace_roots(workspace_id);
```

## 106.4 Files, paths, versions

```sql
CREATE TABLE files (
    file_id             TEXT PRIMARY KEY,            -- stable logical identity (FS-005)
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    root_id             TEXT NOT NULL REFERENCES workspace_roots(root_id),
    current_path        TEXT,                        -- NULL when deleted
    fs_identity         TEXT,                        -- inode+dev / FileId; may be NULL
    current_version_id  TEXT,
    tier_state          TEXT NOT NULL DEFAULT 'RESIDENT' CHECK (tier_state IN
                          ('RESIDENT','PLACEHOLDER','HYDRATING','UNAVAILABLE')),  -- TIER-001
    origin              TEXT NOT NULL DEFAULT 'USER' CHECK (origin IN ('USER','SELF')), -- §98.4
    origin_txn_id       TEXT,                        -- set when origin = SELF
    external_source_url TEXT,                        -- META-004 download origin
    status              TEXT NOT NULL DEFAULT 'ACTIVE'
                          CHECK (status IN ('ACTIVE','DELETED','EXCLUDED','ERROR','FORGOTTEN')),
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);
CREATE INDEX idx_files_ws_status   ON files(workspace_id, status);
CREATE INDEX idx_files_path        ON files(workspace_id, current_path);
CREATE INDEX idx_files_fs_identity ON files(root_id, fs_identity);
CREATE INDEX idx_files_tier        ON files(workspace_id, tier_state) WHERE tier_state != 'RESIDENT';

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
                          CHECK (status IN ('CURRENT','HISTORICAL','TOMBSTONED'))
);
CREATE INDEX idx_versions_file    ON file_versions(file_id, status);
CREATE UNIQUE INDEX idx_versions_current ON file_versions(file_id) WHERE status = 'CURRENT';
CREATE INDEX idx_versions_hash    ON file_versions(content_hash);   -- FS-008 dedup
```

## 106.5 Parsing and IR

```sql
CREATE TABLE parse_results (
    parse_id            TEXT PRIMARY KEY,
    version_id          TEXT NOT NULL REFERENCES file_versions(version_id) ON DELETE CASCADE,
    parser_id           TEXT NOT NULL,
    parser_version      TEXT NOT NULL,               -- PAR-003
    parser_tier         TEXT NOT NULL CHECK (parser_tier IN ('T1','T2','T3','T4','T5')),
    provenance_class    TEXT NOT NULL CHECK (provenance_class IN
                          ('EXACT','DEGRADED','APPROXIMATE','METADATA_ONLY')),  -- CONV-003
    outcome             TEXT NOT NULL CHECK (outcome IN
                          ('OK','PARTIAL','LOW_YIELD','FAILED','UNSUPPORTED','SKIPPED_POLICY')),
    char_yield          INTEGER, page_count INTEGER,
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
    source_span         TEXT NOT NULL CHECK (json_valid(source_span)),
                        -- {page,bbox} | {sheet,range} | {xpath} | {byte_start,byte_end} | {t_start,t_end}
    attributes          TEXT CHECK (attributes IS NULL OR json_valid(attributes)),
    text_hash           TEXT,                        -- CHK-007 IR diffing
    trust               TEXT NOT NULL DEFAULT 'UNTRUSTED_CONTENT' CHECK (trust IN
                          ('DETERMINISTIC_RUNTIME','UNTRUSTED_CONTENT'))   -- PAR-014
);
CREATE INDEX idx_ir_version ON ir_nodes(version_id, ordinal);
CREATE INDEX idx_ir_parent  ON ir_nodes(parent_node_id);
CREATE INDEX idx_ir_kind    ON ir_nodes(version_id, kind);
```

## 106.6 Tables (`TBL`, Part 5 §99)

```sql
CREATE TABLE table_ir (
    table_id            TEXT PRIMARY KEY,
    version_id          TEXT NOT NULL REFERENCES file_versions(version_id) ON DELETE CASCADE,
    node_id             TEXT REFERENCES ir_nodes(node_id),
    source_span         TEXT NOT NULL CHECK (json_valid(source_span)),
    n_rows              INTEGER NOT NULL,
    n_cols              INTEGER NOT NULL,
    header_rows         INTEGER NOT NULL DEFAULT 0,
    header_cols         INTEGER NOT NULL DEFAULT 0,
    header_confidence   REAL NOT NULL DEFAULT 1.0,   -- TBL-003
    column_types        TEXT CHECK (column_types IS NULL OR json_valid(column_types)),
    column_units        TEXT CHECK (column_units IS NULL OR json_valid(column_units)),
    merged_regions      TEXT CHECK (merged_regions IS NULL OR json_valid(merged_regions)),
    caption             TEXT,
    footnotes           TEXT,
    extraction_method   TEXT NOT NULL,               -- native_ooxml | pdf_ruled | pdf_coord | ocr_recon
    provenance_class    TEXT NOT NULL CHECK (provenance_class IN ('EXACT','DEGRADED','APPROXIMATE')),
    confidence          REAL NOT NULL DEFAULT 1.0
);
CREATE INDEX idx_table_version ON table_ir(version_id);

CREATE TABLE table_cells (
    cell_id             TEXT PRIMARY KEY,
    table_id            TEXT NOT NULL REFERENCES table_ir(table_id) ON DELETE CASCADE,
    row_idx             INTEGER NOT NULL,
    col_idx             INTEGER NOT NULL,
    rowspan             INTEGER NOT NULL DEFAULT 1,
    colspan             INTEGER NOT NULL DEFAULT 1,
    raw_text            TEXT,                        -- TBL-005: always retained
    typed_value         TEXT,
    value_type          TEXT,
    unit                TEXT,
    formula             TEXT,                        -- PAR-007
    number_format       TEXT,
    cell_span           TEXT CHECK (cell_span IS NULL OR json_valid(cell_span)),
    confidence          REAL NOT NULL DEFAULT 1.0,
    UNIQUE(table_id, row_idx, col_idx)
);
CREATE INDEX idx_cells_table ON table_cells(table_id, row_idx);
```

## 106.7 Chunks, vectors, indexes

```sql
CREATE TABLE chunks (
    chunk_id            TEXT PRIMARY KEY,
    version_id          TEXT NOT NULL REFERENCES file_versions(version_id) ON DELETE CASCADE,
    root_node_id        TEXT REFERENCES ir_nodes(node_id),
    table_id            TEXT REFERENCES table_ir(table_id),
    chunk_kind          TEXT NOT NULL DEFAULT 'TEXT' CHECK (chunk_kind IN
                          ('TEXT','CODE','TABLE_BAND','TABLE_SCHEMA','TRANSCRIPT',
                           'IMAGE_DESCRIPTION','OCR_TEXT','METADATA')),
    text                TEXT NOT NULL,
    context_prefix      TEXT,                        -- CHK-002 parent headings
    token_count         INTEGER NOT NULL,
    text_hash           TEXT NOT NULL,               -- content-addressed embedding cache (EMB-008)
    chunker_version     TEXT NOT NULL,
    provenance_class    TEXT NOT NULL DEFAULT 'EXACT',
    extraction_method   TEXT NOT NULL DEFAULT 'NATIVE',   -- NATIVE | OCR | ASR | VLM | T3
    status              TEXT NOT NULL DEFAULT 'ACTIVE'
                          CHECK (status IN ('ACTIVE','SUPERSEDED','TOMBSTONED'))
);
CREATE INDEX idx_chunks_version ON chunks(version_id, status);
CREATE INDEX idx_chunks_hash    ON chunks(text_hash);

CREATE TABLE embedding_models (
    model_id            TEXT PRIMARY KEY,
    provider            TEXT NOT NULL,
    model_name          TEXT NOT NULL,
    model_version       TEXT NOT NULL,
    dimension           INTEGER NOT NULL,
    multilingual        INTEGER NOT NULL DEFAULT 0,
    data_boundary       TEXT NOT NULL CHECK (data_boundary IN ('LOCAL','PRIVATE','CLOUD')),
    licence             TEXT, licence_url TEXT, sha256 TEXT     -- LIC-002
);

CREATE TABLE vector_generations (       -- EMB-003 / §20.3
    generation_id       TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    model_id            TEXT NOT NULL REFERENCES embedding_models(model_id),
    state               TEXT NOT NULL CHECK (state IN ('BUILDING','ACTIVE','RETIRING','RETIRED')),
    created_at          INTEGER NOT NULL,
    activated_at        INTEGER
);
CREATE UNIQUE INDEX idx_vecgen_active ON vector_generations(workspace_id) WHERE state = 'ACTIVE';

CREATE TABLE chunk_vectors (            -- mapping only; vectors live in the VectorStore
    chunk_id            TEXT NOT NULL REFERENCES chunks(chunk_id) ON DELETE CASCADE,
    generation_id       TEXT NOT NULL REFERENCES vector_generations(generation_id) ON DELETE CASCADE,
    vector_ref          TEXT NOT NULL,
    embedded_at         INTEGER NOT NULL,
    PRIMARY KEY (chunk_id, generation_id)
);

CREATE TABLE index_generations (        -- Tantivy schema generations (IDX-006)
    generation_id       TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    index_kind          TEXT NOT NULL CHECK (index_kind IN ('LEXICAL')),
    schema_version      INTEGER NOT NULL,
    state               TEXT NOT NULL CHECK (state IN ('BUILDING','ACTIVE','RETIRING','RETIRED')),
    doc_count           INTEGER NOT NULL DEFAULT 0,
    created_at          INTEGER NOT NULL
);
```

## 106.8 Knowledge graph

```sql
CREATE TABLE evidence (
    evidence_id         TEXT PRIMARY KEY,
    version_id          TEXT NOT NULL REFERENCES file_versions(version_id),
    node_id             TEXT REFERENCES ir_nodes(node_id),
    chunk_id            TEXT REFERENCES chunks(chunk_id),
    cell_id             TEXT REFERENCES table_cells(cell_id),
    extractor           TEXT NOT NULL,
    extractor_version   TEXT NOT NULL,
    extraction_method   TEXT NOT NULL,               -- NATIVE|OCR|ASR|VLM|T3|EXIF|AST|GIT
    observed_at         INTEGER NOT NULL,
    content_hash        TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'ACTIVE'
                          CHECK (status IN ('ACTIVE','STALE','TOMBSTONED'))
);
CREATE INDEX idx_evidence_version ON evidence(version_id, status);

CREATE TABLE entities (
    entity_id           TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    entity_type         TEXT NOT NULL,   -- PERSON|ORG|PROJECT|LOCATION|DEVICE|SOFTWARE|
                                         -- MEDIA_ASSET|TRANSCRIPT_SEGMENT|KEYFRAME|SYMBOL|FILE|...
    canonical_name      TEXT NOT NULL,
    attributes          TEXT CHECK (attributes IS NULL OR json_valid(attributes)),
    canonical_entity_id TEXT REFERENCES entities(entity_id),   -- §11.4 equivalence, not destruction
    status              TEXT NOT NULL DEFAULT 'ACTIVE'
                          CHECK (status IN ('ACTIVE','MERGED','TOMBSTONED')),
    origin_principal_id TEXT REFERENCES principals(principal_id),
    created_at          INTEGER NOT NULL
);
CREATE INDEX idx_entities_ws_type ON entities(workspace_id, entity_type, status);
CREATE INDEX idx_entities_name    ON entities(workspace_id, canonical_name);

CREATE TABLE entity_aliases (
    alias_id            TEXT PRIMARY KEY,
    entity_id           TEXT NOT NULL REFERENCES entities(entity_id) ON DELETE CASCADE,
    normalized_alias    TEXT NOT NULL,
    source              TEXT NOT NULL,
    UNIQUE(entity_id, normalized_alias)
);
CREATE INDEX idx_alias_lookup ON entity_aliases(normalized_alias);

CREATE TABLE entity_merges (            -- KG-007 reversible
    merge_id            TEXT PRIMARY KEY,
    left_entity_id      TEXT NOT NULL REFERENCES entities(entity_id),
    right_entity_id     TEXT NOT NULL REFERENCES entities(entity_id),
    canonical_entity_id TEXT NOT NULL REFERENCES entities(entity_id),
    basis               TEXT NOT NULL,
    confidence          REAL NOT NULL,
    actor               TEXT NOT NULL CHECK (actor IN ('USER','MODEL','RULE')),
    created_at          INTEGER NOT NULL,
    reversed_at         INTEGER
);

CREATE TABLE mentions (
    mention_id          TEXT PRIMARY KEY,
    entity_id           TEXT NOT NULL REFERENCES entities(entity_id) ON DELETE CASCADE,
    evidence_id         TEXT NOT NULL REFERENCES evidence(evidence_id) ON DELETE CASCADE,
    surface_form        TEXT NOT NULL,
    span                TEXT CHECK (span IS NULL OR json_valid(span)),
    confidence          REAL NOT NULL DEFAULT 1.0
);
CREATE INDEX idx_mentions_entity   ON mentions(entity_id);
CREATE INDEX idx_mentions_evidence ON mentions(evidence_id);

CREATE TABLE relations (
    relation_id         TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    source_entity_id    TEXT NOT NULL REFERENCES entities(entity_id),
    predicate           TEXT NOT NULL,
    target_entity_id    TEXT NOT NULL REFERENCES entities(entity_id),
    authority_class     TEXT NOT NULL CHECK (authority_class IN
                          ('USER_ASSERTION','DETERMINISTIC_FACT','EXTRACTED_FACT',
                           'INFERRED_FACT','HYPOTHESIS')),                     -- §70.1
    confidence          REAL NOT NULL,
    evidence_id         TEXT REFERENCES evidence(evidence_id),
    valid_from          INTEGER, valid_to INTEGER, observed_at INTEGER NOT NULL,
    superseded_by       TEXT REFERENCES relations(relation_id),
    status              TEXT NOT NULL DEFAULT 'ACTIVE'
                          CHECK (status IN ('ACTIVE','CONFLICTING','STALE','TOMBSTONED','REJECTED')),
    origin_principal_id TEXT REFERENCES principals(principal_id)
);
CREATE INDEX idx_rel_source ON relations(source_entity_id, predicate, status);
CREATE INDEX idx_rel_target ON relations(target_entity_id, predicate, status);

CREATE TABLE facts (
    fact_id             TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    subject_entity_id   TEXT NOT NULL REFERENCES entities(entity_id),
    predicate           TEXT NOT NULL,
    object_value        TEXT NOT NULL CHECK (json_valid(object_value)),
    authority_class     TEXT NOT NULL CHECK (authority_class IN
                          ('USER_ASSERTION','DETERMINISTIC_FACT','EXTRACTED_FACT',
                           'INFERRED_FACT','HYPOTHESIS')),
    confidence          REAL NOT NULL,
    evidence_id         TEXT REFERENCES evidence(evidence_id),
    valid_from          INTEGER, valid_to INTEGER, observed_at INTEGER NOT NULL,
    superseded_by       TEXT REFERENCES facts(fact_id),
    status              TEXT NOT NULL DEFAULT 'ACTIVE'
                          CHECK (status IN ('ACTIVE','CONFLICTING','STALE','TOMBSTONED','REJECTED')),
    origin_principal_id TEXT REFERENCES principals(principal_id)
);
CREATE INDEX idx_facts_subject ON facts(subject_entity_id, predicate, status);

CREATE TABLE communities (
    community_id        TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    level               INTEGER NOT NULL,
    parent_community_id TEXT REFERENCES communities(community_id),
    algorithm           TEXT NOT NULL, algorithm_version TEXT NOT NULL,
    computed_at         INTEGER NOT NULL
);

CREATE TABLE community_members (
    community_id        TEXT NOT NULL REFERENCES communities(community_id) ON DELETE CASCADE,
    entity_id           TEXT NOT NULL REFERENCES entities(entity_id) ON DELETE CASCADE,
    PRIMARY KEY (community_id, entity_id)
);

CREATE TABLE summaries (
    summary_id          TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    scope_kind          TEXT NOT NULL CHECK (scope_kind IN
                          ('DOCUMENT','FOLDER','PROJECT','COMMUNITY','WORKSPACE')),
    scope_ref           TEXT NOT NULL,
    text                TEXT NOT NULL,
    input_refs          TEXT NOT NULL CHECK (json_valid(input_refs)),   -- TMP-005
    model_id            TEXT, template_version TEXT NOT NULL,
    created_at          INTEGER NOT NULL,
    stale_since         INTEGER,
    UNIQUE(workspace_id, scope_kind, scope_ref, template_version)
);
CREATE INDEX idx_summaries_stale ON summaries(workspace_id) WHERE stale_since IS NOT NULL;

CREATE TABLE corrections (              -- KG-005, EVAL-007
    correction_id       TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    target_kind         TEXT NOT NULL CHECK (target_kind IN
                          ('ENTITY','RELATION','FACT','MERGE','SUMMARY','DESCRIPTION','TABLE')),
    target_id           TEXT NOT NULL,
    action              TEXT NOT NULL CHECK (action IN ('CONFIRM','REJECT','EDIT','SPLIT','MERGE')),
    payload             TEXT CHECK (payload IS NULL OR json_valid(payload)),
    origin_principal_id TEXT NOT NULL REFERENCES principals(principal_id),
    created_at          INTEGER NOT NULL
);
CREATE INDEX idx_corrections_target ON corrections(target_kind, target_id);
```

## 106.9 Timeline and media derivatives

```sql
CREATE TABLE activity_events (
    event_id            TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    event_type          TEXT NOT NULL,   -- FILE_CREATED|MODIFIED|RENAMED|DELETED|GIT_COMMIT|
                                         -- ACTION_EXECUTED|EXIF_CAPTURED|APPROVAL|POLICY_DENIAL
    file_id             TEXT REFERENCES files(file_id),
    entity_id           TEXT REFERENCES entities(entity_id),
    event_time          INTEGER NOT NULL,     -- when it happened
    observed_time       INTEGER NOT NULL,     -- when we noticed
    evidence_id         TEXT REFERENCES evidence(evidence_id),
    attributes          TEXT CHECK (attributes IS NULL OR json_valid(attributes))
);
CREATE INDEX idx_events_ws_time ON activity_events(workspace_id, event_time DESC);
CREATE INDEX idx_events_file    ON activity_events(file_id, event_time DESC);

CREATE TABLE media_derivatives (        -- OCR / ASR / VLM / keyframes
    derivative_id       TEXT PRIMARY KEY,
    version_id          TEXT NOT NULL REFERENCES file_versions(version_id) ON DELETE CASCADE,
    kind                TEXT NOT NULL CHECK (kind IN
                          ('OCR_PAGE','ASR_SEGMENT','SUBTITLE_SEGMENT','IMAGE_DESCRIPTION','KEYFRAME')),
    source_span         TEXT NOT NULL CHECK (json_valid(source_span)),  -- page | t_start/t_end | frame
    content             TEXT,
    structured          TEXT CHECK (structured IS NULL OR json_valid(structured)),  -- IMG-003
    engine              TEXT NOT NULL, engine_version TEXT NOT NULL,
    confidence          REAL,
    authority_class     TEXT NOT NULL CHECK (authority_class IN
                          ('EXTRACTED_FACT','HYPOTHESIS')),   -- captions are HYPOTHESIS (§70.1)
    generation_id       TEXT,                                 -- IMG-006
    created_at          INTEGER NOT NULL,
    UNIQUE(version_id, kind, source_span, engine, engine_version)   -- OCR-014 cache key
);
CREATE INDEX idx_media_version ON media_derivatives(version_id, kind);
```

## 106.10 Jobs

```sql
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
    lease_owner         TEXT, lease_expires_at INTEGER,
    not_before          INTEGER NOT NULL DEFAULT 0,  -- backoff
    last_error_code     TEXT, last_error_detail TEXT,
    payload             TEXT CHECK (payload IS NULL OR json_valid(payload)),
    created_at          INTEGER NOT NULL, updated_at INTEGER NOT NULL,
    UNIQUE(idempotency_key)
);
CREATE INDEX idx_jobs_queue ON jobs(status, priority, not_before)
    WHERE status IN ('PENDING','LEASED');
CREATE INDEX idx_jobs_ws    ON jobs(workspace_id, status);
```

## 106.11 Policy, actions, execution, audit

```sql
CREATE TABLE policy_rules (
    rule_id             TEXT PRIMARY KEY,
    scope_kind          TEXT NOT NULL CHECK (scope_kind IN ('GLOBAL','WORKSPACE','ENTERPRISE')),
    scope_ref           TEXT,
    subject             TEXT NOT NULL,               -- tool id | capability | executable
    effect              TEXT NOT NULL CHECK (effect IN
                          ('ALLOW','DENY','REQUIRE_APPROVAL','ALLOW_WITH_REDACTION','ALLOW_WITH_SANDBOX')),
    conditions          TEXT CHECK (conditions IS NULL OR json_valid(conditions)),
    precedence          INTEGER NOT NULL,            -- enterprise deny > requirement > user > default
    signature           TEXT,                        -- signed enterprise policy (§22.1)
    created_at          INTEGER NOT NULL
);
CREATE INDEX idx_policy_lookup ON policy_rules(scope_kind, scope_ref, subject, precedence DESC);

CREATE TABLE model_registry (           -- LLM-001
    model_id            TEXT PRIMARY KEY,
    runtime             TEXT NOT NULL,               -- mistralrs|candle|llamacpp|ollama|openai_compat|cloud
    family              TEXT NOT NULL, params_b REAL, quantization TEXT,
    footprint_mb        INTEGER, context_limit INTEGER,
    min_capability_tier TEXT NOT NULL,
    supports_tools      INTEGER NOT NULL DEFAULT 0,          -- probed (LLM-006)
    supports_structured INTEGER NOT NULL DEFAULT 0,          -- probed (LLM-005)
    supports_vision     INTEGER NOT NULL DEFAULT 0,
    multilingual        INTEGER NOT NULL DEFAULT 0,
    data_boundary       TEXT NOT NULL CHECK (data_boundary IN ('LOCAL','PRIVATE','CLOUD')),
    licence             TEXT, licence_url TEXT, commercial_use INTEGER,   -- LIC-001/003
    sha256              TEXT, installed INTEGER NOT NULL DEFAULT 0,
    probed_at           INTEGER
);

CREATE TABLE action_transactions (
    txn_id              TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id),
    user_request_id     TEXT NOT NULL,
    recipe_id           TEXT,
    model_id            TEXT, provider TEXT,
    plan_hash           TEXT NOT NULL,
    risk_level          TEXT NOT NULL CHECK (risk_level IN ('R0','R1','R2','R3','R4','R5')),
    reversibility       TEXT NOT NULL CHECK (reversibility IN
                          ('REVERSIBLE','COMPENSATABLE','IRREVERSIBLE','UNKNOWN')),   -- §47.1
    approval_state      TEXT NOT NULL CHECK (approval_state IN
                          ('NOT_REQUIRED','PENDING','APPROVED','DENIED','EXPIRED')),
    status              TEXT NOT NULL CHECK (status IN
                          ('PLANNING','AUTHORIZING','PREPARED','EXECUTING','VALIDATING',
                           'COMMITTED','ROLLED_BACK','PARTIAL_FAILURE','CANCELLED')),
    origin_principal_id TEXT NOT NULL REFERENCES principals(principal_id),
    started_at          INTEGER NOT NULL, completed_at INTEGER
);
CREATE INDEX idx_txn_ws ON action_transactions(workspace_id, started_at DESC);

CREATE TABLE action_steps (
    step_id             TEXT PRIMARY KEY,
    txn_id              TEXT NOT NULL REFERENCES action_transactions(txn_id) ON DELETE CASCADE,
    ordinal             INTEGER NOT NULL,
    tool_id             TEXT NOT NULL,
    canonical_targets   TEXT NOT NULL CHECK (json_valid(canonical_targets)),
    arguments_digest    TEXT NOT NULL,
    reversibility       TEXT NOT NULL,
    validator_id        TEXT,                        -- §47.2 mandatory for mutations
    precondition_hash   TEXT,                        -- §25 stale-write check
    before_state        TEXT, after_state TEXT,
    validator_result    TEXT CHECK (validator_result IS NULL OR json_valid(validator_result)),
    rollback_handle     TEXT,
    status              TEXT NOT NULL CHECK (status IN
                          ('PENDING','PREPARED','EXECUTED','VALIDATED','FAILED','ROLLED_BACK','SKIPPED')),
    started_at          INTEGER, completed_at INTEGER,
    UNIQUE(txn_id, ordinal)
);

CREATE TABLE action_snapshots (         -- §47.4
    snapshot_id         TEXT PRIMARY KEY,
    txn_id              TEXT NOT NULL REFERENCES action_transactions(txn_id) ON DELETE CASCADE,
    step_id             TEXT REFERENCES action_steps(step_id),
    original_path       TEXT NOT NULL,
    snapshot_path       TEXT NOT NULL,
    content_hash        TEXT NOT NULL,               -- BLAKE3 of the pre-image
    size_bytes          INTEGER NOT NULL,
    created_at          INTEGER NOT NULL, expires_at INTEGER
);

CREATE TABLE recipes (                  -- EXEC-002 / D37
    recipe_id           TEXT PRIMARY KEY,
    workspace_id        TEXT REFERENCES workspaces(workspace_id),
    name                TEXT NOT NULL,
    version             INTEGER NOT NULL DEFAULT 1,
    dag                 TEXT NOT NULL CHECK (json_valid(dag)),
    author              TEXT NOT NULL CHECK (author IN ('USER','AGENT')),
    max_reversibility   TEXT NOT NULL,
    created_at          INTEGER NOT NULL, updated_at INTEGER NOT NULL,
    UNIQUE(workspace_id, name, version)
);

CREATE TABLE exec_allowlist (           -- EXEC-008
    entry_id            TEXT PRIMARY KEY,
    workspace_id        TEXT NOT NULL REFERENCES workspaces(workspace_id) ON DELETE CASCADE,
    canonical_executable TEXT NOT NULL,
    argv_pattern        TEXT,
    tier                TEXT NOT NULL CHECK (tier IN ('E2','E3','E4')),
    network_allowed     INTEGER NOT NULL DEFAULT 0,
    effect              TEXT NOT NULL CHECK (effect IN ('ALLOW','ASK','DENY')),  -- CAP-003
    created_at          INTEGER NOT NULL,
    UNIQUE(workspace_id, canonical_executable, argv_pattern)
);

CREATE TABLE audit_events (             -- §22.3, SEC-018
    audit_id            TEXT PRIMARY KEY,
    workspace_id        TEXT REFERENCES workspaces(workspace_id),
    category            TEXT NOT NULL,   -- PERMISSION|EGRESS|MCP_AUTH|MUTATION|APPROVAL|
                                         -- POLICY_DENIAL|CORRECTION|SECURITY|EXEC
    actor               TEXT NOT NULL CHECK (actor IN ('USER','AGENT','SYSTEM','EXTERNAL_CLIENT')),
    subject             TEXT NOT NULL,
    decision            TEXT,
    target_digest       TEXT,            -- hashed, not raw content
    detail              TEXT CHECK (detail IS NULL OR json_valid(detail)),
    origin_principal_id TEXT REFERENCES principals(principal_id),
    created_at          INTEGER NOT NULL
);
CREATE INDEX idx_audit_ws_time ON audit_events(workspace_id, created_at DESC);
CREATE INDEX idx_audit_category ON audit_events(category, created_at DESC);
```

## 106.12 Invariants — where each is enforced

| Invariant | Enforced by |
|---|---|
| One CURRENT version per file | Partial unique index `idx_versions_current` |
| One ACTIVE vector generation per workspace | Partial unique index `idx_vecgen_active` |
| Job idempotency | `UNIQUE(idempotency_key)` |
| Parse idempotency | `UNIQUE(version_id, parser_id, parser_version)` |
| OCR/ASR cache | `UNIQUE(version_id, kind, source_span, engine, engine_version)` |
| No fact without provenance (ADR-006) | **Application-level** — `evidence_id` is nullable only for `USER_ASSERTION`; enforced by a trigger and a test |
| Captions cannot be facts (§70.1) | **Application-level** — `media_derivatives.authority_class` limited to `HYPOTHESIS` for `IMAGE_DESCRIPTION`; enforced by CHECK + test |
| `SELF` content excluded from evidence authority (§98.4) | **Application-level** — retrieval and §46.3 filter on `files.origin`; enforced by an adversarial test |
| Mutation requires a validator (§47.2) | **Application-level** — tool registration fails at startup if a mutation tool has no `validator_id` |
| Cross-user isolation (MULTI) | Process and filesystem boundary, not SQL. Every workspace is reachable only through the owning principal's daemon |

**Rule:** every application-level invariant above has a named test in §116.3. An invariant with no test is a comment.

---

# 107. Migration and versioning

| Rule |
|---|
| Migrations are forward-only, numbered, and each has an explicit `up`. `down` is optional; the recovery mechanism is the backup, not the reverse migration. |
| `VACUUM INTO` a timestamped backup before any migration (§50). Migration failure restores the backup and refuses to start writes (NFR-011). |
| **Schema back-compat window: one minor version** (PKG-010). A newer schema must be readable-but-frozen by the previous app version, or the app must refuse to open it with a clear message — never silently. |
| Derived stores (Tantivy, vectors) are **never** migrated in place. Build a new generation, verify, switch the pointer atomically, retire the old (§20.3). |
| A schema change that invalidates derived data sets `processor_version`, which naturally re-queues the affected jobs. No manual reindex trigger. |
| Migrations run single-threaded through the writer actor with all workers paused. |
| Every migration has a test that runs it against a fixture database from each supported prior version. |
| Canonical metadata must survive a full derived-index rebuild without losing corrections or policies (NFR-012) — tested, not assumed. |

---

# 108. Error taxonomy

Every error is `{code, category, severity, retryable, user_message, detail}`. `user_message` is the SUP-001 cause-and-action string; `detail` is for diagnostics and is scrubbed.

| Prefix | Category | Retryable | Example codes |
|---|---|---|---|
| `FS_` | Filesystem | Sometimes | `FS_PERMISSION_DENIED`, `FS_LOCKED`, `FS_NOT_FOUND`, `FS_PLACEHOLDER_SKIPPED`, `FS_VOLUME_UNAVAILABLE`, `FS_PATH_ESCAPE_BLOCKED` |
| `PAR_` | Parsing | Once | `PAR_UNSUPPORTED`, `PAR_CORRUPT`, `PAR_TIMEOUT`, `PAR_LOW_YIELD`, `PAR_TRUNCATED`, `PAR_BUDGET_EXCEEDED`, `PAR_WORKER_CRASH` |
| `IDX_` | Index | Yes | `IDX_CORRUPT`, `IDX_GENERATION_MISMATCH`, `IDX_REBUILD_REQUIRED` |
| `EMB_` | Embedding | Yes | `EMB_MODEL_UNAVAILABLE`, `EMB_DIM_MISMATCH`, `EMB_BACKLOG_FULL` |
| `MOD_` | Model gateway | Sometimes | `MOD_UNAVAILABLE`, `MOD_TIMEOUT`, `MOD_CONTEXT_EXCEEDED`, `MOD_SCHEMA_INVALID`, `MOD_OOM`, `MOD_NO_TOOL_SUPPORT` |
| `POL_` | Policy | **No** | `POL_DENIED`, `POL_APPROVAL_REQUIRED`, `POL_CLASSIFICATION_BLOCKED`, `POL_NETWORK_BLOCKED`, `POL_ENTERPRISE_DENY` |
| `ACT_` | Action / transaction | No | `ACT_STALE_VERSION`, `ACT_VALIDATOR_FAILED`, `ACT_ROLLBACK_FAILED`, `ACT_IRREVERSIBLE_PARTIAL`, `ACT_BUDGET_EXCEEDED` |
| `EXEC_` | Execution | No | `EXEC_NOT_ALLOWLISTED`, `EXEC_SANDBOX_UNAVAILABLE`, `EXEC_LIMIT_EXCEEDED`, `EXEC_TIER_DISABLED` |
| `HW_` | Hardware / capability | No | `HW_INSUFFICIENT_MEMORY`, `HW_NO_ACCELERATOR`, `HW_THERMAL_THROTTLE`, `HW_ON_BATTERY` |
| `TBL_` | Table | No | `TBL_UNIT_MISMATCH`, `TBL_HEADER_AMBIGUOUS`, `TBL_RECONSTRUCTION_FAILED`, `TBL_JOIN_KEY_REQUIRED` |
| `DB_` | Storage | Sometimes | `DB_BUSY`, `DB_CORRUPT`, `DB_MIGRATION_FAILED`, `DB_DISK_FULL` |
| `IPC_` | Transport | Yes | `IPC_VERSION_MISMATCH`, `IPC_PEER_REJECTED`, `IPC_CANCELLED`, `IPC_PAYLOAD_TOO_LARGE` |
| `MCP_` | Interop | Sometimes | `MCP_SERVER_UNAVAILABLE`, `MCP_CAPABILITY_DENIED`, `MCP_DEPRECATED_FEATURE` |
| `ENT_` | Entitlement | No | `ENT_EXPIRED`, `ENT_SEAT_EXCEEDED`, `ENT_TIER_REQUIRED` |

| Rule |
|---|
| `POL_*` is **never retryable and never auto-escalated.** A denial that a retry can defeat is not a policy. |
| Every `PAR_*` isolates to one file; the workspace continues (FS-011). |
| Every error surfaced to a user carries a specific `user_message` naming the file class or root involved. Generic failure text is a defect (SUP-001). |
| Error codes are stable identifiers. They appear in telemetry (TEL-003 permits counters), documentation and support tooling. |

---

# 109. Configuration reference

`<AppData>/marrow/config/settings.json` — **non-secret only** (§19). Secrets live in the OS keyring (SEC-005), entitlement included (ENT-010).

```jsonc
{
  "schema_version": 1,
  "app": { "locale": "auto", "theme": "system", "telemetry_opt_in": false },
  "runtime": {
    "network_mode": "normal",              // normal | local_only   (SEC-015)
    "max_parser_workers": "auto",
    "writer_batch_rows": 500,
    "writer_batch_ms": 100
  },
  "scheduler": {                            // §21
    "profile": "balanced",                  // balanced | battery_saver | max_speed
    "idle_threshold_ms": 180000,
    "require_ac_for_tier_c": true,
    "require_ac_for_media": true,
    "require_ac_for_generation": true
  },
  "indexing": {
    "follow_symlinks": false,               // WS-005
    "index_hidden": false,
    "respect_gitignore": true,
    "max_file_bytes_full_parse": 104857600,
    "archive": { "max_entries": 10000, "max_expanded_bytes": 2147483648, "max_depth": 4 },
    "tiered_storage": { "hydrate": "never", "warn_threshold_files": 100 }   // TIER-005
  },
  "models": {
    "embedding": { "preferred": null, "allow_cloud": false },
    "generation": { "preferred": null, "allow_cloud": false, "prefer_detected_runtime": true },
    "auto_recommend": true                  // LLM-002
  },
  "retrieval": {
    "fusion": "rrf",                        // §113
    "rrf_k": 60,
    "candidates_per_branch": 100,
    "max_context_tokens": 8000,
    "max_chunks_per_file": 3,
    "min_source_diversity": 3
  },
  "execution": { "tier": "E1", "allow_network_runners": false },   // EXEC-001
  "generation": { "rendering_enabled": true, "diffusion_enabled": false },
  "privacy": {
    "egress_confirm_above_bytes": 32768,
    "location_extraction": false,           // META-003 / D17 — default off
    "media": { "ocr": false, "captions": false, "asr": false }
  },
  "budgets": { "monthly_cloud_cost_ceiling_usd": null, "run_max_steps": 25,
               "run_max_seconds": 600, "run_max_tokens": 200000 }
}
```

| Rule |
|---|
| Every key has a default; a missing file is valid and yields defaults. |
| Unknown keys are preserved on write (forward compatibility across the PKG-010 window). |
| Enterprise managed policy (§22.1) **overrides** this file and is displayed as read-only in the UI with its source named. |
| No secret, token, key or licence is ever written here (SEC-005, ENT-010) — tested by a scanner in CI. |
| A config change that invalidates derived state enqueues jobs; it never silently produces inconsistent results. |

---

# 110. IPC contract

Adopting the §94.2 A1 finding: a **registered action/query CQRS surface with generated TypeScript types**, rather than a hand-maintained message list that drifts.

## 110.1 Transport

| Item | Value |
|---|---|
| macOS/Linux | Unix domain socket, user-private dir, mode `0700`, path includes UID + session (MULTI-003) |
| Windows | Named pipe, name includes SID + session ID; DACL restricted to that SID |
| Framing | 4-byte big-endian length prefix + MessagePack body |
| Auth | `SO_PEERCRED` / `LOCAL_PEERCRED` / `GetNamedPipeClientProcessId` + token check (§49) |
| Never | Localhost TCP for privileged operations |

## 110.2 Envelope

```jsonc
// Request
{ "request_id": "01J...", "protocol_version": 3, "kind": "query|action|subscribe|cancel",
  "name": "search.hybrid", "workspace_id": "01J...", "payload": { } }

// Response — one of
{ "request_id": "...", "type": "result",   "payload": { } }
{ "request_id": "...", "type": "progress", "payload": { "stage": "...", "done": 12, "total": 40 } }
{ "request_id": "...", "type": "stream",   "seq": 3, "payload": { } }
{ "request_id": "...", "type": "error",    "error": { "code": "POL_DENIED", "user_message": "..." } }
{ "request_id": "...", "type": "end" }
```

| Rule |
|---|
| **Queries** are side-effect free and freely cancellable. **Actions** mutate and route through the policy engine without exception. |
| Authorization derives from peer credentials and daemon state. Client-supplied identity fields are ignored (§49). |
| Every request carries `request_id`; `cancel` references it. Cancellation is cooperative and must be honoured within 500 ms. |
| Protocol version mismatch → `IPC_VERSION_MISMATCH` with an explicit upgrade instruction. |
| Payload size cap; oversized payloads are rejected, not truncated. |
| **The registry is the source of truth.** TypeScript types are generated from the Rust definitions in CI; a drift check fails the build. |

## 110.3 Surface catalogue

| Name | Kind | Notes |
|---|---|---|
| `workspace.list` / `.get` / `.create` / `.update` / `.pause` / `.remove` / `.forget` | q/a | `forget` is §26, irreversible, explicit confirmation |
| `workspace.addRoot` / `.removeRoot` / `.rootHealth` | q/a | Health includes watcher state (WATCH-008) and tier counts (TIER-008) |
| `search.lexical` / `.semantic` / `.hybrid` / `.literal` | q | `literal` is CAP-005, index-independent |
| `search.explain` | q | RET-004 trace |
| `file.get` / `.intelligence` / `.preview` / `.versions` / `.reparse` / `.ocr` | q/a | `.intelligence` is the §99.1 `FI` read model |
| `table.list` / `.get` / `.compute` / `.export` | q | `.compute` is TBL-008 |
| `knowledge.entity` / `.neighborhood` / `.timeline` / `.facts` / `.evidence` | q | Bounded traversal (KG-011) |
| `knowledge.correct` | a | Becomes `USER_ASSERTION` |
| `answer.ask` | subscribe | Streams tokens + citations + verification state |
| `agent.plan` / `.start` / `.approve` / `.deny` / `.cancel` / `.get` / `.rollback` | q/a | §30.3 |
| `recipe.list` / `.get` / `.save` / `.run` / `.preview` | q/a | §97.3 |
| `exec.allowlist.*` | q/a | CAP-003 |
| `model.list` / `.recommend` / `.install` / `.remove` / `.probe` | q/a | LLM block; `.recommend` explains itself (LLM-015) |
| `hardware.profile` / `.reprobe` | q/a | HW-004 |
| `index.health` / `.rebuild` / `.reconcile` | q/a | §16.10 |
| `policy.effective` | q | Shows the resolved decision and which rule produced it |
| `audit.query` / `.export` | q | §22.3 |
| `diagnostics.bundle` | a | §23.3, reviewable before send (SUP-002) |
| `settings.get` / `.set` | q/a | §109 |
| `events.subscribe` | subscribe | Index progress, job state, approvals pending |

---

# 111. Job system detail

## 111.1 State machine

```text
PENDING --lease--> LEASED --start--> RUNNING --ok--> DONE
   ^                  |                  |
   |            lease expiry             fail
   |                  |                  |
   +------------------+<-----------------+
                      |
              attempt >= max_attempts
                      |
                      v
                    DEAD  (surfaced in index health, never retried silently)
```

| Rule |
|---|
| Leases are time-bounded; an expired lease returns the job to `PENDING` (crash recovery, NFR-003). |
| Backoff is exponential with jitter, written to `not_before`. |
| `DEAD` jobs are visible in index health with their error code and a manual retry action — never hidden. |
| A job is enqueued by `idempotency_key`; re-enqueue is a no-op (§20.2). |
| Priority is dynamic: a file entering an answer's evidence set is promoted to P1 (§48.2). |
| Cancellation is checked at every checkpoint; long jobs (video, §67) checkpoint progress (VID-012). |

## 111.2 Dependencies

Rather than a general DAG, jobs chain by emission — each job enqueues its successor on success. This keeps the queue flat and restart-safe:

```text
SCAN_ROOT → PROBE_FILE → HASH_FILE → PARSE_FILE → { INDEX_TEXT, CHUNK_FILE, EXTRACT_METADATA }
CHUNK_FILE → EMBED_CHUNKS → EXTRACT_ENTITIES → RESOLVE_ENTITIES → UPDATE_GRAPH → BUILD_SUMMARY
```

Failure of a downstream job never invalidates upstream results, because each is separately idempotent and separately keyed.

---

# 112. Chunking algorithm

Satisfies CHK-001..008. Format-aware, not fixed-token.

| Step | Rule |
|---|---|
| 1. Unit selection | Walk the IR, not the text. Candidate units are semantic: heading section, code symbol, table band, slide, transcript window, list, paragraph group |
| 2. Never split | Code symbols (CHK-004), table rows, merged cell regions, transcript speaker turns |
| 3. Target size | Model-aware, stored independently of provider (CHK-008). Default target 512 tokens, hard max 1024, min 64 (merge upward below min) |
| 4. Overlap | **Structural, not sliding.** A chunk carries its ancestor headings as `context_prefix` rather than duplicating neighbour text. Cheaper and more precise than fixed overlap |
| 5. Tables | Band by rows to the target size; **repeat header rows and caption on every band**; emit one additional `TABLE_SCHEMA` chunk describing columns, types, units and value ranges |
| 6. Code | One chunk per top-level symbol where it fits; oversized symbols split at statement boundaries with a signature prefix |
| 7. Transcripts | Speaker turn, or a time window when turns are unavailable (VID-007); never fixed token counts |
| 8. Stable IDs | `chunk_id` derived from `(version lineage, root_node_id, ordinal)`; unchanged structure yields unchanged IDs (CHK-003) |
| 9. Diffing | On re-parse, match by `text_hash` first, then by node path. Unchanged chunks retain their IDs **and their vectors** (CHK-007, §10.2) |
| 10. Dedup | Identical `text_hash` reuses the cached embedding (EMB-007/008) unless the surrounding context materially differs |

---

# 113. Retrieval fusion baseline

§12.2 correctly refuses to freeze weights. But "do not hard-code weights" is not implementable without a starting point. This is the **v1 baseline to be measured and replaced**, not a claim of optimality.

## 113.1 Pipeline

```text
query
  → intent classification (§12.1)
  → branch selection (only relevant branches run)
  → per-branch candidate retrieval (default 100 each)
  → RRF fusion
  → feature scoring / boosts
  → optional cross-encoder rerank (top 30)
  → context builder (§114)
```

## 113.2 Fusion

Reciprocal Rank Fusion, rank-based so branch scores need no normalization:

```text
score(d) = Σ_branches  w_b / (k + rank_b(d))      k = 60 (default)
```

| Branch | Default `w_b` | Runs when |
|---|---|---|
| Lexical (BM25) | 1.0 | Always |
| Vector | 1.0 | Semantic/global intent, and embeddings exist |
| Path/filename | 0.8 | Always (cheap) |
| Symbol/exact | 1.2 | Code intent, or query looks like an identifier |
| Graph proximity | 0.7 | An entity resolved from the query |
| Timeline recency | 0.6 | Temporal intent |
| Table schema | 0.9 | Query mentions a column, unit or aggregate |

## 113.3 Post-fusion multipliers

| Signal | Effect |
|---|---|
| Exact filename or symbol match | ×1.5 (IDX-004) |
| User pin or correction | ×2.0 |
| Provenance class `DEGRADED` | ×0.8 (CONV-005) |
| Provenance class `APPROXIMATE` | ×0.6 |
| `origin = SELF` (§98.4) | ×0.5 **and excluded from evidence authority** |
| Evidence `STALE` | ×0.3 |
| Recency, temporal intent only | ×(1 + 0.4·decay(age)) |

## 113.4 Tuning protocol

| Rule |
|---|
| Weights live in config, not code, and are versioned. |
| Any weight change runs the golden query set in CI; a Recall@10 regression blocks merge (EVAL-004). |
| A learned reranker is considered **only after** the RRF baseline is measured. Replacing an unmeasured baseline teaches nothing. |
| `search.explain` renders the per-branch ranks and multipliers for any result (RET-004). |

---

# 114. Context envelope serialization

This is the concrete mechanism behind ADR-007 and §6.2. Prose about "labelled untrusted evidence" is not implementable; this is.

## 114.1 Construction rules

| Rule |
|---|
| The system prompt is assembled **only** by the runtime from templates in the binary. No retrieved text ever reaches it. |
| Evidence is serialized into structured blocks with runtime-generated, unpredictable delimiters — never Markdown fences an attacker can close. |
| Every block declares its `trust`, `source` and `id`. The model cites by `id`. |
| Untrusted text is escaped so it cannot terminate its own block; a delimiter collision re-generates the delimiter. |
| Block order is: system → user request → deterministic facts → untrusted evidence → tool schemas. Untrusted content is never last, so it cannot be the final instruction. |
| **The prompt is defence in depth, not the control.** The policy engine enforces the same rules independently and would block the action even if the model complied fully with injected text (§6.2 rule 3). |

## 114.2 Shape

```text
<<<Marrow:SYS:7f3a91c4>>>
role=system
(runtime template only)
<<<Marrow:END:7f3a91c4>>>

<<<Marrow:FACT:7f3a91c4>>>
id=F1  trust=DETERMINISTIC_RUNTIME  source=parser:xlsx@2.1  span={sheet:"Q2",range:"B4:B18"}
sum(B4:B18) = 148320.00 USD
<<<Marrow:END:7f3a91c4>>>

<<<Marrow:EVIDENCE:7f3a91c4>>>
id=E1  trust=UNTRUSTED_CONTENT  provenance=EXACT  external=false  origin=USER
source=file:01J8.../v3  span={page:17,bbox:[72,410,520,566]}
"...the agreement renews on 31 December 2026 unless either party..."
<<<Marrow:END:7f3a91c4>>>
```

| Field | Purpose |
|---|---|
| `trust` | `DETERMINISTIC_RUNTIME` \| `UNTRUSTED_CONTENT` \| `USER` |
| `provenance` | `EXACT` \| `DEGRADED` \| `APPROXIMATE` (CONV-003) — drives the citation badge |
| `external` | META-004 download origin; elevates injection scrutiny (§71.2) |
| `origin` | `USER` \| `SELF` — `SELF` cannot support a claim (§98.4) |
| `id` | The citation handle. §46.3 binds claims to these |

## 114.3 Budgeting

Applied in order, each stage measured:

| Order | Policy |
|---|---|
| 1 | Drop evidence whose classification forbids this provider (MOD-004) — **before** any token accounting |
| 2 | Collapse duplicates by `text_hash` (RET-008) |
| 3 | Enforce max chunks per file (default 3) for source diversity (RET-009) |
| 4 | Enforce minimum distinct sources where available (default 3) |
| 5 | Expand parent headings into `context_prefix` |
| 6 | Cap graph neighbourhood expansion (KG-011) |
| 7 | Trim to the token budget, lowest fused score first |
| 8 | Secret/DLP scan on the assembled envelope (SEC-007) — **after** assembly, because concatenation can reveal what fragments did not |
| 9 | Record byte count and source count for the egress disclosure (UX-013) |

---

# 115. Prompt and template governance

| Rule |
|---|
| Every prompt template is a versioned file in the repository. No prompt is constructed by string concatenation at a call site. |
| `template_version` is persisted with every derived artifact — summaries, extractions, descriptions (§13.1, IMG-005). |
| A template change is a **generation change** for anything it produced (LLM-012). Prior outputs are not retroactively attributed to the new template. |
| Templates for structured output ship with their JSON Schema; output is validated before it touches the graph or a tool (MOD-009). |
| Every template has golden-output tests against fixed inputs on a pinned model; drift fails CI. |
| Templates are reviewed as security-relevant code. A template change that weakens the untrusted-content framing requires security review. |
| Templates are localizable where user-visible text is embedded (I18N-007). |

---

# 116. Test strategy and CI

## 116.1 Layers

| Layer | Scope | Runs |
|---|---|---|
| Unit | Pure logic: path canonicalization, chunk boundaries, type inference, fusion math, sizing arithmetic | Every commit |
| Property | Round-trips: IR→chunk→IR, table IR→export→import, path normalization idempotence, ULID ordering | Every commit |
| Integration | Real SQLite, real Tantivy, real vector store, real files on a temp volume | Every commit |
| Corpus | The permissioned corpus (EVAL-001): parse yield, table accuracy, retrieval golden set | Every PR touching those paths |
| **Adversarial** | **§29.4 + §71.5 + §97.6 + §98.4 injection, path, archive, codec, exec, self-poisoning** | **Every commit — non-negotiable** |
| Soak | 72-hour watcher/reconciliation drift; memory growth; job-queue crash recovery | Nightly |
| Performance | §119 budgets on pinned hardware | Nightly + release |
| Platform | Per-OS: watcher semantics, path forms, NFC/NFD (I18N-009), placeholder detection, MULTI isolation | Per release train |
| Upgrade | Migration from each supported prior version; PKG-010 back-compat window | Per release |
| Accessibility | Keyboard traversal, screen-reader labels on approval dialogs (A11Y-002) | Per release |

## 116.2 Adversarial corpus — the permanent set

| # | Case | Must result in |
|---|---|---|
| 1 | Hostile instruction in a PDF | Indexed as text; zero authority granted |
| 2 | `README` asking the agent to upload keys | Policy denial; security event recorded |
| 3 | Symlink to `~/.ssh` inside a trusted root | Path escape blocked at operation time (SEC-002) |
| 4 | Zip-slip archive | Extraction aborted (SEC-010) |
| 5 | Decompression bomb (archive and image) | Budget abort (PAR-010, IMG-014) |
| 6 | Command injection via filename | Argv never interpolated; execution blocked |
| 7 | Malicious MCP tool description | Not trusted as instruction (MCP-007) |
| 8 | Stale-file race before write | Write rejected (§25) |
| 9 | Tool result containing fake approval text | No approval effect |
| 10 | Injection inside a screenshot (OCR text) | `OCR_DERIVED`, zero authority (M1) |
| 11 | Injection inside EXIF comment | Length-capped, escaped, data only (M4) |
| 12 | Injection inside subtitles/ASR | Untrusted channel (M3) |
| 13 | Adversarial image against the VLM | Caption is `HYPOTHESIS`; no action derivable (M7) |
| 14 | Malformed video/image → codec fuzz | Sandboxed worker dies alone; file marked failed (M6) |
| 15 | **Self-poisoning: agent writes a summary, then cites it** | **Refused — `SELF` origin excluded from evidence authority (§98.4)** |
| 16 | **Env-var exfiltration via a runner** | **Blocked by env allowlist (EXEC-009)** |
| 17 | **Fork bomb / runaway process** | **Killed by process-count and CPU limits (EXEC-011)** |
| 18 | **Runner attempts network egress** | **Denied by default (EXEC-010)** |
| 19 | **Recipe whose steps escalate beyond approved scope** | **Re-resolution + policy denial (EXEC-006)** |
| 20 | Cloud placeholder file touched by any code path | Never hydrated (TIER-005) |
| 21 | Another OS user connects to the daemon socket | Rejected on peer credentials (MULTI-004) |
| 22 | Table with mismatched units summed | Blocked with `TBL_UNIT_MISMATCH` (TBL-009) |

**Rule:** this set only grows. Every I1-class incident (SIR-005) and every reproduced security defect adds a permanent case.

## 116.3 Invariant tests (from §106.12)

| Test | Asserts |
|---|---|
| `no_fact_without_provenance` | Every non-`USER_ASSERTION` fact/relation has a live `evidence_id` |
| `captions_cannot_be_facts` | No `IMAGE_DESCRIPTION` derivative ever produces a relation or fact |
| `self_origin_excluded` | A `SELF` file is searchable but never binds a claim in §46.3 |
| `every_mutation_has_validator` | Startup registration fails otherwise |
| `secrets_never_in_config` | Scanner over `settings.json` and all logs |
| `diagnostics_have_no_bodies` | Bundle builder output scanned for file content and foreign paths |
| `ungated_features_unreachable_from_entitlement` | ENT-011 |
| `derived_rebuild_preserves_corrections` | NFR-012 |
| `no_deprecated_mcp_features` | MCP-012 lint |

## 116.4 CI gates

| Gate | Blocks merge |
|---|---|
| Unit + property + integration green | ✅ |
| Adversarial corpus: **zero escapes** | ✅ |
| Invariant tests green | ✅ |
| Golden retrieval set: no Recall@10 regression | ✅ (EVAL-004) |
| Clippy, fmt, deny-warnings | ✅ |
| Licence audit: no GPL/AGPL in shipped binary without clearance | ✅ (LIC-005) |
| SBOM generated; no known-critical CVE unpatched past SLA | ✅ (SIR-007/008) |
| Rust↔TypeScript type drift check | ✅ (§110.2) |
| Installer size budget (PKG-001) | ✅ |
| Perf budgets (§119) | Nightly, release-blocking |

---

# 117. Build and release engineering

| Item | Rule |
|---|---|
| Reproducible builds | Pinned toolchain, locked dependencies, vendored where practical. A given tag produces byte-identical artifacts on the CI image |
| Platform matrix | macOS arm64 + x64, Windows x64 + arm64, Linux x64 (+ arm64 later) |
| Signing | macOS codesign + notarization + stapling; Windows Authenticode (EV where possible); Linux detached signature. Keys in HSM (SIR-002) |
| Update | Delta binaries (PKG-008); signed manifests; rollback protection; **never during an open transaction (PKG-009)** |
| Channels | `stable`, `beta`, `nightly`. Schema changes ride the same window rules (PKG-010) on all channels |
| Model artifacts | Content-addressed, integrity-verified, served with size shown before download (PKG-011/013) |
| Grammars | Top-10 Tree-sitter grammars bundled, rest lazily fetched and verified (PKG-004) |
| Sidecar | Frozen Python built per platform, signed, dependency lockfile audited (CONV-011) |
| Air-gapped | Offline installer with models bundled (PKG-006) and offline activation (ENT-005), built from the same tag |
| Uninstall | Offers keep / delete-index / delete-everything (PKG-012); never touches another user's data (MULTI-010) |
| Versioning | SemVer for the app; independent integer versions for schema, IPC protocol, parser, chunker, embedding generation, prompt templates |
| Release checklist | §83.3 launch gates re-verified per release train, not once at launch |

---

# 118. Frontend architecture

| Concern | Decision |
|---|---|
| Shell | Tauri 2; narrow capability scopes; no unrestricted FS access from the WebView (SEC-012) |
| Framework | React + TypeScript |
| Server state | TanStack Query over generated IPC clients (§110.2) — no hand-written request code |
| Client state | Zustand for UI-only state. **Never mirror daemon state into a client store**; the daemon is the source of truth |
| Streaming | `events.subscribe` feeds a single event bus; components subscribe by topic |
| Virtualization | TanStack Virtual for results, timeline, and the file intelligence panel |
| Editor/diff | CodeMirror or Monaco for diffs and previews |
| Graph | Cytoscape/Sigma, **bounded neighbourhoods only** (§16.5) |
| Charts | The G1 renderer (GEN-001); every rendered mark carries its source ref for click-through (GEN-002) |
| Design system | Tokens for colour, spacing, type; risk and authority are encoded as **text + icon, never colour alone** (A11Y-003) |
| Accessibility | Keyboard-complete; approval dialogs read target paths aloud (A11Y-002); reduced-motion and high-contrast respected (A11Y-004); WCAG AA contrast (A11Y-005); graph has a list equivalent (A11Y-006) |
| Localization | All strings externalized from day one (I18N-007); RTL layout support (I18N-004) |
| Performance | Search results render progressively; **no spinner blocks lexical results waiting on a model** (§16.3) |

## 118.1 Surface-to-capability map

| Surface | Primary IPC | Key requirements |
|---|---|---|
| Ask | `answer.ask` | UX-005, UX-012, §46.3 verification state, citation click-through |
| Search | `search.hybrid`, `.literal` | UX-004, §16.3 match reasons, CAP-005 literal mode |
| File intelligence | `file.intelligence` | §99.1, FI-001..008 |
| Knowledge | `knowledge.*` | §16.5, §16.6 corrections |
| Activity | `audit.query`, `events.subscribe` | UX-016 |
| Workspaces | `workspace.*`, `index.health` | WS-006, §16.10, WATCH-008, TIER-008 |
| Agents | `recipe.*`, `agent.*` | §16.7, §16.8, EXEC tiers visible |
| Settings | `settings.*`, `model.*`, `hardware.profile` | UX-012, HW-004, LLM-015, BGT-005 |

---

# 119. Consolidated performance budgets

| Operation | Target | Source | Measured on |
|---|---|---|---|
| Filename/metadata search p95 | < 50 ms | NFR-007 | Warm index, 100k files |
| Lexical search p95 | < 100 ms | NFR-008 | Same |
| Hybrid retrieval p95 (excl. LLM) | < 500 ms | NFR-009 | Same |
| Literal scan (CAP-005), 10k files | < 3 s | New | Cold page cache |
| File intelligence panel p95 | < 250 ms | New (FI/R41) | Warm |
| Table compute (TBL-008), ≤ 10k cells | < 200 ms | New | Warm |
| Scan + metadata, 100k files | < 20–30 min | §50, §56 P1 | SSD |
| SQLite batched write throughput | 5–20k rows/s | §50 | |
| Watcher event → index visible | < 5 s | New | Live watcher |
| Reconciliation drift, 72 h soak | 0 | §56 P1 | |
| IPC round trip, small query | < 5 ms | New | |
| Cancellation honoured | < 500 ms | §110.2 | |
| HW probe | < 2 s | HW-002 | |
| Cold app launch to usable search | < 3 s | New | Warm OS cache |
| Idle memory (daemon, indexed, idle) | < 400 MB | New | 100k files |
| Idle CPU (no backlog) | < 1% | §21 | |
| Base installer | ≤ 500 MB | PKG-001 rev. (§102) | |
| Total after default models | ≤ 1.4 GB | PKG-002 (§72.3) | |

**Rule:** every budget has a nightly benchmark. A regression opens a release-blocking issue automatically. A budget with no benchmark is deleted from this table rather than left as an aspiration.

---

# 120. Glossary

| Term | Meaning |
|---|---|
| **Authority class** | Rank of a fact's trustworthiness: `USER_ASSERTION` > `DETERMINISTIC_FACT` > `EXTRACTED_FACT` > `INFERRED_FACT` > `HYPOTHESIS` (§11.2, §70.1) |
| **Chunk** | Retrieval unit derived from IR nodes, carrying source location and context prefix |
| **Community** | Cluster of related entities used for global/corpus-wide queries (§13) |
| **Compensatable** | An action with an inverse that is not an exact restoration (§47.1) |
| **Evidence** | A provenance record binding a fact to a file version, node and extraction method |
| **Generation** | A versioned build of a derived index; reads target the active one while the next builds (§20.3) |
| **IR** | Intermediate representation — the parser's structured, versioned output (§8.6) |
| **Provenance class** | Precision of a citation: `EXACT` \| `DEGRADED` \| `APPROXIMATE` (CONV-003) |
| **Recipe** | Declarative DAG of typed tools; E1 execution with no process spawn (§97.3) |
| **Reconciliation** | Periodic full comparison of filesystem truth against the index; watchers are hints (§2.6) |
| **Reversibility class** | `Reversible` \| `Compensatable` \| `Irreversible` \| `Unknown` (§47.1) |
| **Risk class** | R0–R5, from metadata read to external side effect (§14.1) |
| **`SELF` origin** | Content Marrow itself wrote; searchable but barred from supporting claims (§98.4) |
| **Tier A/B/C** | Extraction tiers: deterministic / lightweight semantic / LLM (§11.1) |
| **Tier E0–E4** | Execution tiers, from none to arbitrary shell (§97.1) |
| **Tier T1–T5** | Parser tiers by provenance fidelity (§63) |
| **Tier T-min…T-max** | Hardware capability tiers (§95.3) |
| **Tiered storage** | Cloud-backed placeholder files that must never be silently hydrated (§45.1) |
| **Untrusted evidence** | Any content originating outside the runtime; carries no authority, ever (ADR-007) |
| **Validator** | The post-mutation check that decides whether a step is committed or rolled back (§46.4) |
| **Workspace** | A consented set of roots with its own policy, classification and budgets |

---

# 121. Consolidated indexes

## 121.1 Requirement blocks

| Prefix | Topic | Count | Part | Phase |
|---|---|---|---|---|
| WS | Workspace / consent | 12 | 1 | P1 |
| FS | Filesystem discovery | 16 | 1 | P1 |
| PAR | Parsing | 14 | 1 | P1 |
| CHK | Chunking | 8 | 1 | P2 |
| IDX | Lexical index | 7 | 1 | P1 |
| EMB | Embeddings | 10 | 1 | P2 |
| KG | Knowledge graph | 14 | 1 | P4 |
| TMP | Temporal | 6 | 1 | P5 |
| RET | Retrieval | 12 | 1 | P2 |
| MOD | Model gateway | 9 | 1 | P2 |
| AGT | Agent / tools | 15 | 1 | P3 |
| MCP | Interop | 12 | 1 + 2 | P7 |
| SEC | Security | 18 | 1 | P1→ |
| UX | Experience | 16 | 1 | P1→ |
| NFR | Reliability / perf | 12 | 1 | P1→ |
| TIER | Tiered storage | 12 | 2 | **P1** |
| WATCH | Watcher limits | 10 | 2 | **P1** |
| MAIL | Email | 10 | 2 | V1.5–V2 |
| I18N | Language | 9 | 2 | P1→ |
| PKG | Packaging | 12 | 2 | P1→ |
| LIC | Licensing | 7 | 2 | P1 |
| EVAL | Evaluation | 10 | 2 | P0→ |
| SYNC | Portability | 7 | 2 | P1 |
| TEL | Telemetry | 6 | 2 | P2 |
| A11Y | Accessibility | 6 | 2 | P3 |
| BGT | Budget | 6 | 2 | P2 |
| CONV | Conversion tiers | 12 | 3 | P2 |
| OCR | OCR | 14 | 3 | P5 |
| IMG | Image understanding | 15 | 3 | P5 |
| VID | Video | 12 | 3 | P6 |
| AUD | Audio | 10 | 3 | P6 |
| META | Embedded metadata | 10 | 3 | **P1** |
| MULTI | Multi-user | 15 | 4 | **P1** / P3 |
| ENT | Entitlement | 12 | 4 | P2 |
| SUP | Support | 12 | 4 | P1→ |
| SIR | Security IR | 10 | 4 | P1→ |
| CMP | Compliance | 15 | 4 | P1→ |
| DPA | Data processing | 10 | 4 | P2→ |
| HW | Hardware probe | 10 | 5 | **P1** |
| LLM | Local models | 15 | 5 | P2 |
| EXEC | Execution | 20 | 5 | P3 → P6 |
| GEN | Generative media | 14 | 5 | P5 |
| FI | File intelligence | 8 | 5 | **P1** |
| TBL | Tables | 18 | 5 | P1 → P5 |
| CAP | Agent parity | 12 | 5 | P3 |
| **Total** | | **~520** | | |

## 121.2 Decisions

| Range | Topic | Source | Status |
|---|---|---|---|
| D1–D9 | Stack benchmarks (vector store, embedding runtime, FTS, PDF, platform, model, daemon split, graph threshold, clustering) | §59 | Open — P0/P4 |
| D10 | Business model | §59 | **Resolved** — §80 |
| D11 | Competitive positioning | §59 | **Resolved** — §82.4 |
| D12–D18 | Sidecar, OCR engine, VLM, ASR, ffmpeg, GPS default, sidecar removal | §75.1 | Open |
| D19–D30 | Commercial, compliance, distribution | §90 | Open |
| D31–D40 | Runtime, execution tiers, tables, recipes, literal search | §103.1 | Open |

## 121.3 Risks

| Range | Source | Highest-severity members |
|---|---|---|
| R1–R14 | §55 | R3 placeholder hydration, R6 injection reaching a mutation |
| R15–R22 | §75.2 | R18 screenshot injection, R19 codec RCE |
| R23–R32 | §91 | R23 shared-account leakage, R29 OS vendor ships provenance, R31 overstated sandbox |
| R33–R42 | §103.2 | **R39 self-poisoning**, R38 wrong table numbers, R40 sandbox slip |

## 121.4 ADRs

ADR-001..010 (§33) stand unchanged. Part 5 adds three:

| ADR | Decision | Reason |
|---|---|---|
| **ADR-011** | Execution is tiered E0–E4; E1 recipes require no OS process | Delivers scripting value without waiting on sandbox work that has historically slipped (§51, R40) |
| **ADR-012** | Numeric answers are computed over the Table IR, not read from flattened text by a model | Removes the largest remaining source of confidently wrong figures; makes §46.3 numeric verification pass by construction |
| **ADR-013** | All agent-written and generated content is marked `SELF` and barred from evidence authority | Prevents the system corroborating its own output (§98.4) |

---

# 122. Documentation map

| Part | Title | Covers | Read when |
|---|---|---|---|
| 1 | Master Specification (§1–43) | Vision, requirements, architecture, security, data flows, stack, roadmap, ADRs | First. Everything else assumes it |
| 2 | Gap Closure, Verification, Cost (§44–60) | Tiered storage, watcher limits, verification subsystem, reversibility, Tier C governor, IPC auth, SQLite writes, honest sandbox posture, MCP corrections, revised plan, cost, risks | Before committing to a plan or a date |
| 3 | Conversion, OCR, Multimodal (§61–76) | Parser tiers, MarkItDown sidecar, OCR, images, video, audio, media metadata, multimodal threats | Before touching parsing or media |
| 4 | Multi-User, Commercial, Compliance, Ops (§77–92) | `MULTI`, SKUs, pricing, entitlement, competition, GTM, support, security IR, compliance, DPAs, total cost | Before commercial or legal commitments; §78 before P1 schema |
| 5 | Execution, Local Inference, Generative Media, File Intelligence, Parity (§93–104) | Prior art, `HW`, `LLM`, `EXEC`, `GEN`, `FI`, `TBL`, `CAP`, answer coverage | Before building execution, local models, tables or the agent loop |
| 6 | Engineering Reference (§105–122) | Schema DDL, migrations, errors, config, IPC, jobs, chunking, fusion, context envelope, prompts, tests, release, frontend, budgets, glossary, indexes | While implementing. This is the build-from document |

## 122.1 Maintenance rules

| Rule |
|---|
| Numbering is append-only. A superseded section is annotated, never renumbered. |
| A requirement ID, once issued, is never reused. Withdrawn requirements are marked `WITHDRAWN` with a reason. |
| Every new requirement states its phase and its verifying test. |
| Cost, timeline and market figures are re-baselined at each phase gate (§56), not silently carried forward. |
| Anything tagged `[ASSUMPTION]` or `[COUNSEL]` in Part 4 is either validated and retagged, or removed. |
| Conflicts resolve in favour of the **later** part, because the later parts exist to correct the earlier ones. |
| Sections 106–119 are the ones that go stale against code. Treat drift between them and the repository as a defect in this document. |
