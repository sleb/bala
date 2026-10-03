-- Bala schema baseline (docs/design/data-store.md §Schema).
--
-- Some columns and tables are not read by `bala-core` yet
-- (`dependency_edges`; `tasks.assignee_id`, `out_of_sync`, `completed_at`,
-- `deleted_at`). They are part of the designed schema so the features that
-- use them need no table rebuild.

CREATE TABLE task_types (
    key         TEXT PRIMARY KEY,
    label       TEXT NOT NULL,
    color       TEXT,
    sort_order  INTEGER NOT NULL
);

CREATE TABLE users (
    id    BLOB PRIMARY KEY,   -- 16-byte UUID
    name  TEXT NOT NULL
);

CREATE TABLE tasks (
    id            BLOB PRIMARY KEY,          -- 16-byte UUID
    title         TEXT NOT NULL,
    description   TEXT,
    type_key      TEXT NOT NULL REFERENCES task_types(key),
    status        TEXT NOT NULL CHECK (status IN ('incomplete', 'complete')),
    start_date    TEXT,                      -- ISO-8601 date
    due_date      TEXT,
    assignee_id   BLOB REFERENCES users(id),
    out_of_sync   INTEGER NOT NULL DEFAULT 0, -- 0/1
    created_at    TEXT NOT NULL,              -- RFC 3339 UTC
    updated_at    TEXT NOT NULL,
    completed_at  TEXT,
    deleted_at    TEXT                        -- soft-delete tombstone; NULL = live
);
-- progress is library-computed on read (Core LLD §Algorithm 4) — no column.

CREATE INDEX idx_tasks_type_live     ON tasks(type_key)     WHERE deleted_at IS NULL;
CREATE INDEX idx_tasks_status_live   ON tasks(status)       WHERE deleted_at IS NULL;
CREATE INDEX idx_tasks_assignee_live ON tasks(assignee_id)  WHERE deleted_at IS NULL;

-- Every task has exactly one parent edge, and every edge a sibling position.
--
--  * The edge names the task's one parent; `parent_id IS NULL` means the
--    task is at the top level.
--  * `position` (0-based, per parent; NULL parent = the root list) orders
--    siblings.
--  * "At most one edge per task" is enforced by the unique index on
--    `child_id`, so a task cannot have two parents, nor be both top level
--    and under a parent. "At least one" is not a constraint: it holds
--    because `set_parent_edge` writes a new task's edge in the transaction
--    that creates it and only ever replaces that edge afterwards.
CREATE TABLE parent_edges (
    parent_id BLOB REFERENCES tasks(id),           -- NULL = top level
    child_id  BLOB NOT NULL REFERENCES tasks(id),
    position  INTEGER NOT NULL
);

CREATE UNIQUE INDEX idx_parent_edges_child ON parent_edges(child_id);     -- one parent per task; parent of X
                                                                          -- (hierarchy upward walk, Core LLD §Algorithm 1)
CREATE INDEX idx_parent_edges_parent_pos ON parent_edges(parent_id, position); -- ordered children of X
                                                                          -- (rollup, subtree delete)

CREATE TABLE dependency_edges (
    predecessor_id BLOB NOT NULL REFERENCES tasks(id),
    successor_id   BLOB NOT NULL REFERENCES tasks(id),
    dep_type       TEXT NOT NULL CHECK (
        dep_type IN ('finish_to_start', 'start_to_start', 'finish_to_finish', 'start_to_finish')
    ),
    PRIMARY KEY (predecessor_id, successor_id)  -- one edge per pair (Core LLD §Data Model);
                                                 -- add_dependency_edge upserts, doesn't duplicate
);
-- PK's leading column (predecessor_id) indexes "what depends on X"
-- (cascade forward-propagation, Core LLD §Algorithm 3; list_successor_edges).
-- Reverse needs its own index:
CREATE INDEX idx_dependency_edges_successor ON dependency_edges(successor_id); -- "what does X depend on"
                                                                                -- (dependency validity walk, Core LLD §Algorithm 2;
                                                                                -- list_dependency_edges)

-- Seed the default "task" TaskType every `Core` expects to exist.
-- `bala-core`'s `Core::new` also seeds it idempotently, but seeding it here
-- keeps a fresh store usable standalone (e.g. from `sqlite3`). `INSERT OR
-- IGNORE` keeps this compatible with `Core::new`'s own check.
INSERT OR IGNORE INTO task_types (key, label, color, sort_order)
VALUES ('task', 'Task', NULL, 0);
