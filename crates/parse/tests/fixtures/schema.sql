CREATE TABLE ir_nodes (
    node_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    source_span TEXT NOT NULL
);

CREATE INDEX idx_ir_kind ON ir_nodes(kind);

CREATE VIEW precise_nodes AS
SELECT node_id FROM ir_nodes WHERE source_span <> 'whole';
