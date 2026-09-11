-- Full 4-table schema per docs/design/data-store.md §Schema.
--
-- `dependency_edges` sits unused until Epic 3's dependency features land,
-- and several `tasks` columns (`assignee_id`, `out_of_sync`, `completed_at`,
-- `deleted_at`) are unused by `bala-core`'s current `Task`/`TreeFilter`
-- shape (Story 1.1 scope cut) — both are included now, per the LLD and
-- this checkpoint's explicit instruction, so the schema doesn't need a
-- breaking migration when those stories land.

CREATE TABLE task_types (
    key         TEXT PRIMARY KEY,
    label       TEXT NOT NULL,
    color       TEXT,
    sort_order  INTEGER NOT NULL
);

CREATE TABLE tasks (
    id            BLOB PRIMARY KEY,          -- 16-byte UUID
    title         TEXT NOT NULL,
    description   TEXT,
    type_key      TEXT NOT NULL REFERENCES task_types(key),
    status        TEXT NOT NULL CHECK (status IN ('incomplete', 'complete')),
    start_date    TEXT,                      -- ISO-8601 date
    due_date      TEXT,
    assignee_id   BLOB,
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

CREATE TABLE parent_edges (
    parent_id BLOB NOT NULL REFERENCES tasks(id),
    child_id  BLOB NOT NULL REFERENCES tasks(id),
    PRIMARY KEY (parent_id, child_id)
);
-- PK's leading column (parent_id) already indexes "children of X"
-- (rollup, subtree delete). Reverse direction needs its own index:
CREATE INDEX idx_parent_edges_child ON parent_edges(child_id); -- "parents of X"
                                                                -- (hierarchy upward walk, §1)

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
-- (cascade forward-propagation, §3; list_successor_edges). Reverse needs
-- its own index:
CREATE INDEX idx_dependency_edges_successor ON dependency_edges(successor_id); -- "what does X depend on"
                                                                                -- (dependency validity walk, §2;
                                                                                -- list_dependency_edges)

-- Seed the default "task" TaskType every `Core` expects to exist
-- (bala-core's `Core::new` also seeds this idempotently, but `bala-store`
-- seeds it too so a fresh store is usable standalone, e.g. from `sqlite3`
-- or a future caller that skips `Core`). `INSERT OR IGNORE` keeps this
-- migration re-runnable/idempotent alongside `Core::new`'s own check.
INSERT OR IGNORE INTO task_types (key, label, color, sort_order)
VALUES ('task', 'Task', NULL, 0);
