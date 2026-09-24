-- Story 3.5 Checkpoint 2: every task gets a parent edge, and every edge a
-- stored sibling position (docs/design/data-store.md, `parent_edges`).
--
--  * `parent_id` becomes nullable: a top-level task has exactly one edge
--    with `parent_id IS NULL`.
--  * `position` (0-based, per parent; NULL parent = the root list) orders
--    siblings.
--  * "exactly one NULL edge XOR one-or-more real edges" is enforced in code
--    (`Parents` + `replace_parent_edges`), not by triggers; the schema only
--    guarantees at most one NULL edge per child and no duplicate real edge.
--
-- SQLite can't drop a PRIMARY KEY / NOT NULL in place, so this is a table
-- rebuild like V2. Nothing references `parent_edges`, so a plain
-- create-copy-drop-rename is safe.
--
-- Backfill: siblings are numbered by the child's `created_at`, then `id`
-- as a stable tie-break. Tasks with no edge at all (live or tombstoned)
-- get a NULL edge, numbered the same way among the roots.

CREATE TABLE parent_edges_new (
    parent_id BLOB REFERENCES tasks(id),           -- NULL = top level
    child_id  BLOB NOT NULL REFERENCES tasks(id),
    position  INTEGER NOT NULL
);

INSERT INTO parent_edges_new (parent_id, child_id, position)
SELECT e.parent_id, e.child_id,
       ROW_NUMBER() OVER (PARTITION BY e.parent_id ORDER BY t.created_at, t.id) - 1
FROM parent_edges e
JOIN tasks t ON t.id = e.child_id;

INSERT INTO parent_edges_new (parent_id, child_id, position)
SELECT NULL, t.id,
       (SELECT COUNT(*) FROM parent_edges_new WHERE parent_id IS NULL)
         + ROW_NUMBER() OVER (ORDER BY t.created_at, t.id) - 1
FROM tasks t
WHERE NOT EXISTS (SELECT 1 FROM parent_edges e WHERE e.child_id = t.id);

DROP TABLE parent_edges;
ALTER TABLE parent_edges_new RENAME TO parent_edges;

CREATE UNIQUE INDEX idx_parent_edges_real_pair ON parent_edges(parent_id, child_id)
    WHERE parent_id IS NOT NULL;
CREATE UNIQUE INDEX idx_parent_edges_one_null ON parent_edges(child_id)
    WHERE parent_id IS NULL;
CREATE INDEX idx_parent_edges_child ON parent_edges(child_id);            -- parents of X
CREATE INDEX idx_parent_edges_parent_pos ON parent_edges(parent_id, position); -- ordered children of X
