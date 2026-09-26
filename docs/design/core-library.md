# LLD-1: Bala — Core Library Low-Level Design

**Status:** Proposed
**Date:** 2026-08-31
**Deciders:** Scott (product/eng)
**Related:** [HLD.md](./HLD.md) (Core Library component), [user stories](https://github.com/sleb/bala/issues?q=label%3Astory) (this library owns CRUD, hierarchy, and dependency/rollup logic; the TUI and the Gantt chart are rendering concerns handled by their respective clients)

## Context

HLD.md fixed the Core Library as the single home for task CRUD, hierarchy
invariants, dependency invariants, cascade scheduling, and progress rollup,
exposed as one method contract that the CLI/TUI calls in-process today and
a future Web API wraps unchanged. It deliberately left language, exact
signatures, and algorithms to this LLD. Three decisions from that list
were resolved before writing this doc, since they change the method
surface itself rather than just its implementation:

- **Language: Rust.** A compiled, statically-typed library that both an
  in-process CLI/TUI and a future HTTP layer can call gives the strongest
  compile-time guarantee that invariants (no circular hierarchy, no
  circular dependency, dates always library-computed) can't be bypassed
  by a caller — see the [rust-best-practices](../../.claude/skills/rust-best-practices)
  guidance applied throughout §4–§6 below.
- **Delete: soft-delete.** `delete_task` sets a `deletedAt` tombstone
  rather than physically removing rows. Tombstoned tasks are excluded
  from normal queries but stay in the graph for a recovery window, so
  hierarchy/dependency invariants never have to reason about a task that
  half-exists.
- **Completion: block-by-default, cascade-on-request** — the `rm` /
  `rm -r` model. `complete_task` rejects completing a parent with
  incomplete children unless the caller explicitly asks for the
  cascading form, which completes the whole subtree. No silent
  auto-complete and no silent "just ignore the children" — the caller
  always gets one of the two explicit behaviors.

This LLD covers the `bala-core` crate: its module layout, data types,
error taxonomy, method contract, and the three algorithms (hierarchy
invariant enforcement, dependency cycle detection, cascade scheduling)
that carry the real complexity per HLD's consequences section.

## Decision

`bala-core` is a single crate with internal module seams matching HLD's
call-out (`hierarchy`, `scheduling`, `rollup` are modules, not crates),
plus a `store` module holding the persistence trait boundary and a
`model` module holding shared types. A `Core` struct is the facade every
caller (CLI today, Web API later) drives.

```mermaid
flowchart TB
    subgraph "bala-core crate"
        Facade["Core (facade struct)"]
        Hierarchy["hierarchy\n(reparent, subtree walk, no-cycle check)"]
        Scheduling["scheduling\n(dependency graph, cascade, out-of-sync)"]
        Rollup["rollup\n(progress computation)"]
        Types["types\n(TaskType config)"]
        Model["model\n(Task, TaskPatch, TaskId, errors)"]
        StoreTrait["store\n(Store / StoreTx traits)"]
    end
    Caller["CLI/TUI today\nWeb API later"] --> Facade
    Facade --> Hierarchy
    Facade --> Scheduling
    Facade --> Rollup
    Facade --> Types
    Hierarchy --> Model
    Scheduling --> Model
    Rollup --> Model
    Facade --> StoreTrait
    StoreTrait -.implemented by.-> DataStore[("bala-store\n(Data Store LLD)")]
```

`Core` is generic over its store — `Core<S: Store>` — rather than boxing
internally: there is exactly one store implementation live at a time (the
embedded engine the Data Store LLD picks), so static dispatch costs
nothing and keeps every call monomorphized. A caller that genuinely needs
a trait object (unlikely — the CLI and the future Web API each construct
one concrete `Core<SqliteStore>` at startup) can still box `Core` itself;
that's a boundary decision, not something `bala-core` should pay for
internally.

## Data Model

```rust
/// Opaque, non-`Copy`-sized but small newtype wrapper — one per domain
/// entity — so a `TaskId` can never be passed where a `UserId` is
/// expected, even though both wrap a `Uuid`.
pub struct TaskId(Uuid);
pub struct UserId(Uuid);

/// Minimal user identity for task assignment — no auth, email, roles, or
/// avatars (out of scope per #9); just enough to give
/// `assignee_id` a real entity to resolve to instead of a bare id.
pub struct User {
    pub id: UserId,
    pub name: String,
}

pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub description: Option<String>,
    pub parent_ids: Vec<TaskId>,    // a task may sit under multiple parents
                                     // (e.g. shared by two goals/projects);
                                     // empty = top-level
    pub type_key: String,           // FK into TaskType.key; "task" default
    pub status: TaskStatus,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub assignee_id: Option<UserId>,
    pub depends_on: Vec<Dependency>, // predecessors, each typed
    pub out_of_sync: bool,
    pub progress: f32,              // 0.0..=1.0, library-computed, read-only
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,  // soft-delete tombstone
}

pub enum TaskStatus { Incomplete, Complete }

pub struct Dependency {
    pub predecessor_id: TaskId,
    pub dep_type: DependencyType,
}

/// One edge per (predecessor, successor) pair — adding a new type between
/// an already-linked pair replaces the existing edge rather than adding a
/// second one, so `remove_dependency` never has to disambiguate by type.
pub enum DependencyType {
    FinishToStart,  // default; predecessor finishes before successor starts
    StartToStart,   // predecessor starts before successor starts
    FinishToFinish, // predecessor finishes before successor finishes
    StartToFinish,  // predecessor starts before successor finishes
}

pub struct TaskType {
    pub key: String,       // stable identifier, e.g. "initiative"
    pub label: String,     // display name, user-renameable
    pub color: Option<String>,
    pub sort_order: i32,
}

/// Filter predicate for `get_tree`/`list_tasks`. All fields are ANDed;
/// `None`/empty means "don't filter on this". Fixes the assumption
/// Data Store LLD made ahead of this definition (its §Open Questions) —
/// field-for-field, matching the indexes it already built for these:
/// `type_key`, `status`, `assignee_id` each get a partial index on
/// `deleted_at IS NULL`, and `include_deleted` toggles that predicate.
/// `assignee_id` filters against a real `User.id` (#9) — `Core`
/// validates it exists via `Store::get_user` at `create_task` time, so a
/// filter value here always corresponds to a real, resolvable user.
pub struct TreeFilter {
    pub type_key: Option<String>,
    pub status: Option<TaskStatus>,
    pub assignee_id: Option<UserId>,
    pub include_deleted: bool,  // default false: soft-deleted tasks excluded
}

impl Default for TreeFilter {
    fn default() -> Self {
        Self { type_key: None, status: None, assignee_id: None, include_deleted: false }
    }
}
```

**`TaskPatch` — the "don't touch" vs. "clear" problem.** A plain
`Option<T>` field on a patch struct is ambiguous: does `None` mean "leave
it alone" or "set it to nothing"? Rather than lean on `Option<Option<T>>`
(compiles, but reads badly at every call site — `Some(None)` is not
self-documenting), `TaskPatch` uses a small explicit enum per nullable
field:

```rust
pub enum Field<T> {
    Keep,
    Set(T),
    Clear,   // only meaningful for Option<T> targets, e.g. description
}

pub struct TaskPatch {
    pub title: Field<String>,
    pub description: Field<String>,   // Clear -> None
    pub start_date: Field<NaiveDate>, // Clear -> None
    pub due_date: Field<NaiveDate>,   // Clear -> None
    pub assignee_id: Field<UserId>,   // Clear -> unassign
    pub type_key: Field<String>,
    // parent_ids and depends_on are intentionally NOT here — reparenting
    // and dependency edits go through their own dedicated methods
    // (set_parents, add/remove_dependency) because each carries its
    // own invariant check that a generic patch would obscure.
}

impl Default for TaskPatch {
    fn default() -> Self { /* every field Field::Keep */ }
}
```

Callers build a patch with struct-update syntax against
`TaskPatch::default()`, touching only what changed — e.g.
`TaskPatch { title: Field::Set("New title".into()), ..Default::default() }`.

## Error Taxonomy

One `thiserror`-derived enum, `CoreError`, per the "structured errors any
caller can render consistently" contract in HLD §Interfaces. No
`anyhow`/string errors cross this boundary — `bala-core` is a library, not
a binary.

```rust
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("task {0:?} not found")]
    NotFound(TaskId),

    #[error("title must not be empty")]
    EmptyTitle,

    #[error("due date {due} is before start date {start}")]
    InvalidDateRange { start: NaiveDate, due: NaiveDate },

    #[error("moving {task:?} under {attempted_parent:?} would make it its own ancestor")]
    CircularHierarchy { task: TaskId, attempted_parent: TaskId },

    #[error("{task:?} is not a child of {parent}")] // parent: id, or "the top level" for None
    NotUnderParent { task: TaskId, parent: Option<TaskId> },

    #[error("{task:?} cannot depend on {other:?}: it is an ancestor/descendant of it")]
    DependsOnRelative { task: TaskId, other: TaskId },

    #[error("adding this dependency would create a cycle: {cycle:?}")]
    CircularDependency { cycle: Vec<TaskId> },

    #[error("{task:?} has incomplete children: {incomplete:?}; pass cascade=true or complete them first")]
    IncompleteChildren { task: TaskId, incomplete: Vec<TaskId> },

    #[error("unknown task type {0:?}")]
    UnknownTaskType(String),

    #[error("user name must not be empty")]
    EmptyUserName,

    #[error("unknown user {0:?}")]
    UnknownUser(UserId),

    #[error(transparent)]
    Store(#[from] StoreError),
}
```

Every variant carries the ids/values needed to render a specific message
without re-querying — matching HLD's "touched tasks" philosophy applied
to errors, not just successes.

## Method Contract (the `Core` facade)

```rust
impl<S: Store> Core<S> {
    pub fn create_user(&mut self, name: String) -> Result<User, CoreError>;
    pub fn list_users(&self) -> Result<Vec<User>, CoreError>;
    pub fn create_task(&mut self, new: NewTask) -> Result<Task, CoreError>;
    pub fn update_task(&mut self, id: TaskId, patch: TaskPatch) -> Result<Vec<Task>, CoreError>;
    pub fn delete_task(&mut self, id: TaskId, mode: DeleteMode) -> Result<DeleteOutcome, CoreError>;
    pub fn restore_task(&mut self, id: TaskId) -> Result<Task, CoreError>;
    pub fn set_parents(&mut self, id: TaskId, new_parents: Vec<TaskId>) -> Result<Task, CoreError>;
    pub fn move_sibling(&mut self, parent: Option<TaskId>, id: TaskId, direction: Direction) -> Result<bool, CoreError>;
    pub fn indent_task(&mut self, parent: Option<TaskId>, id: TaskId) -> Result<bool, CoreError>;
    pub fn outdent_task(&mut self, parent: Option<TaskId>, grandparent: Option<TaskId>, id: TaskId) -> Result<bool, CoreError>;
    pub fn add_dependency(&mut self, id: TaskId, predecessor: TaskId, dep_type: DependencyType) -> Result<Task, CoreError>;
    pub fn remove_dependency(&mut self, id: TaskId, predecessor: TaskId) -> Result<Task, CoreError>;
    pub fn complete_task(&mut self, id: TaskId, cascade: bool) -> Result<Vec<Task>, CoreError>;
    pub fn reopen_task(&mut self, id: TaskId) -> Result<Task, CoreError>;
    pub fn preview_cascade(&self, id: TaskId, patch: TaskPatch) -> Result<Vec<Task>, CoreError>;
    pub fn get_tree(&self, filter: TreeFilter) -> Result<Vec<Task>, CoreError>;
    pub fn get_task(&self, id: TaskId) -> Result<Option<Task>, CoreError>;
    pub fn list_children(&self, id: TaskId) -> Result<Vec<Task>, CoreError>;
    /// Sibling order of every live task, keyed by parent (`None` = top level);
    /// one transaction. `list_children` and `get_tree` follow it: children in
    /// stored position order (creation order by default), `get_tree` results
    /// in depth-first sibling order (a task under several parents takes its
    /// first position; unreached tasks sort last).
    pub fn sibling_order(&self) -> Result<SiblingOrder, CoreError>;
    pub fn list_task_types(&self) -> Result<Vec<TaskType>, CoreError>;
    pub fn upsert_task_type(&mut self, t: TaskType) -> Result<TaskType, CoreError>;
}

pub enum DeleteMode { Subtree, PromoteChildren }

pub struct DeleteOutcome {
    pub deleted: Vec<Task>, // tombstoned by this call
    pub updated: Vec<Task>, // survived, but its own fields changed
}
```

`DeleteMode` and `complete_task`'s `cascade: bool` are the two places this
contract encodes the "rm vs. rm -r" decision from §Context — deliberately
as an explicit parameter rather than two method names, since the CLI/Web
API each map it onto one confirmation prompt either way.

With multiple parents, `DeleteMode::Subtree` only ever removes
*parent-edges*, not tasks: deleting task A with `Subtree` drops the A→X
edge for every child X, and X itself is only soft-deleted (tombstoned)
once that leaves it with an empty `parent_ids` — i.e. A was its only
parent. A child still reachable through another live parent (e.g. it's
also under Goal B) keeps existing (not tombstoned), just with one less
parent edge, so deleting a goal can never silently delete a task that
another goal still needs. `PromoteChildren` follows the same rule in
reverse: A's parent edge on each child is replaced with an edge to A's
own parents (or dropped, making the child top-level, if A had none) —
for a child with other parents besides A, that's one more edge added
alongside the ones it already keeps.

`delete_task` reports what it changed as a `DeleteOutcome` rather than
one mixed list, so no caller has to partition on `deleted_at` itself:
`deleted` holds every task the call tombstoned, `updated` every task that
survived but whose own fields changed — a `Subtree` child that lost the
A→X edge but is still under another parent, or a `PromoteChildren` child
reparented onto A's parents. Both lists hold only tasks whose own stored
row changed, each at most once and at its final value, and no id is in
both: a child that first survives one dropped edge and is later left
parentless by the same walk (e.g. A→X, A→D, D→X) is reported only in
`deleted`. A parent whose rolled-up `progress` changed only because its
children changed (e.g. B, once the shared X loses A) is in neither list;
a caller showing it re-fetches it.

`move_sibling(parent, id, Direction::{Up,Down})` reorders `id` by one
place within `parent`'s children (`None` = top level) in a single
transaction. It swaps with the nearest *live* sibling in that direction
(tombstoned siblings are skipped) via `StoreTx::swap_child_positions`, so
only `parent`'s list changes: `id`'s position under any other parent, its
parents, depth and `updated_at` are untouched. It returns `true` if the task
moved and `false` at the end of the list (a true no-op: no write, no error).
It fails with `NotFound` for a missing/deleted `id` and `NotUnderParent {
task, parent }` when `id` is not a child of `parent`.

`indent_task(parent, id)` / `outdent_task(parent, grandparent, id)` take
the *rendered path* (the parent the row is shown under, plus that parent's
parent for outdent), so in a multi-parent DAG they change only that one
path. Each runs in one transaction, composes the new parent set (current
minus `parent`, plus the target) and writes it through
`hierarchy::replace_parents`, so the cycle guard is shared with
`set_parents` (`CircularHierarchy`); other parents keep their edges and
positions. Both return `true` on a change and `false` on a no-op (no write,
no `updated_at` bump); a change bumps `updated_at` and refreshes
`parent_ids`.

- Indent: the target is the nearest *live* previous sibling under `parent`
  (tombstoned ones skipped); `id` becomes its last child. No previous
  sibling: no-op. Only `id`'s own edges change, so its subtree follows.
- Outdent: `id` joins `grandparent`'s children immediately after `parent`
  (`Placement::After(parent)`). `parent == None` (already top level) is a
  no-op. `NotUnderParent` if `id` is not under `parent`, or `parent` not
  under `grandparent`. With `grandparent == None` the task becomes top level
  only if `parent` was its sole parent; if other real parents remain it
  just drops `parent`, since a task is never both top level and nested.

`set_parents` replaces the old single-parent `reparent_task(id,
Option<TaskId>)`. It takes the *whole* new parent list and applies it
atomically: the hierarchy invariant (§1) is checked for every added
parent before any edge is written, and if any one of them would create a
cycle, the whole call is rejected with `CircularHierarchy` naming that
specific parent — no partial reparenting. This is a bulk replace, not an
incremental add/remove, because the per-edge cycle check doesn't depend
on which other parents are being added in the same call (adding parent P
to task `id` only risks a cycle if `id` is already an ancestor of P,
independent of P's siblings in the call), so bulk-and-atomic gets the
same error precision as one-edge-at-a-time without the extra round
trips.

## Algorithms

### 1. Hierarchy invariant — no circular nesting ([#37](https://github.com/sleb/bala/issues/37) AC5)

Multiple parents make the hierarchy a DAG, not a tree, so "ancestor
chain" becomes "ancestor set reachable via `parent_ids`." `set_parents(id,
new_parents)` checks each candidate `parent` in `new_parents`
independently: DFS/BFS upward from `parent` following `parent_ids`
edges, with a `visited` set (a shared ancestor reachable through two
different paths — e.g. two of `id`'s new parents both rolling up to the
same goal — is only walked once). If `id` is reachable in that walk,
reject the whole call with `CircularHierarchy { task: id, attempted_parent:
parent }` — no edges are written until every candidate parent has passed
the check. O(edges) per call, same complexity class as the old O(depth)
single-chain walk since `visited` bounds the work to each ancestor node
once regardless of how many paths reach it. `create_task` skips the
check and writes the new task's edges directly: a freshly minted id has no
descendants, so it cannot be an ancestor of any entry in
`NewTask.parent_ids`, and each parent's existence is already validated.
Running the walk anyway would cost one `list_parent_edges` per ancestor,
making every create under a deep chain O(depth) for a check that can never
fail. Every write that adds a parent to an *existing* task still goes
through `hierarchy::replace_parents` and its check.

### 2. Dependency invariants ([#60](https://github.com/sleb/bala/issues/60) AC2–3)

`add_dependency(id, predecessor, dep_type)`:
1. Reject if `id == predecessor` (self-dependency).
2. Walk `id`'s ancestor *set* and descendant subtree in the hierarchy
   graph — with multiple parents this is DAG reachability (§1's
   memoized walk) rather than a single chain, in both directions: upward
   from each of `id`'s `parent_ids`, and downward through everything
   reachable as a descendant of `id`. Reject with `DependsOnRelative` if
   `predecessor` appears in either set — this is the one place a
   dependency check must cross into the hierarchy graph, per HLD §4.
3. Cycle check on the dependency graph itself: before adding the edge
   `predecessor → id`, DFS from `id` following existing `depends_on`
   edges outward (i.e., walk what `id` already (transitively) depends
   on). If `predecessor` is reachable, the new edge would close a cycle
   — reject with `CircularDependency { cycle }`, where `cycle` is the
   path found. This is a plain reachability check, not a full
   topological sort, and runs in O(edges) per call. The check doesn't
   depend on `dep_type` — a cycle is a cycle regardless of which of the
   four types closes it.
4. If an edge already exists between `predecessor` and `id` (of any
   type), it is replaced by the new one rather than adding a second edge
   — see §Data Model, `Dependency` — so `add_dependency` is also how a
   caller changes an existing edge's type.

`remove_dependency` has no invariant to check — removing an edge can
never introduce a cycle or a relative-dependency violation.

### 3. Cascade scheduling ([#61](https://github.com/sleb/bala/issues/61) AC1–4)

Each `DependencyType` anchors a different field of the predecessor to a
different field of the successor:

| Type | Constraint | Anchor field (predecessor) | Constrained field (successor) |
|---|---|---|---|
| Finish-to-Start (FS, default) | `successor.start ≥ predecessor.due` | `due_date` | `start_date` |
| Start-to-Start (SS) | `successor.start ≥ predecessor.start` | `start_date` | `start_date` |
| Finish-to-Finish (FF) | `successor.due ≥ predecessor.due` | `due_date` | `due_date` |
| Start-to-Finish (SF) | `successor.due ≥ predecessor.start` | `start_date` | `due_date` |

`constraint_ok(edge, pred, succ)` and `anchor_date(edge, pred)` are the
two small per-type functions everything below is built from — the rest
of the algorithm is type-agnostic once those exist.

Two distinct triggers, kept separate because they have different
observable outcomes:

- **A task's own dates are edited directly** (`update_task` on a task
  that itself has predecessors): for each predecessor edge, if
  `constraint_ok` is now violated, the edit is **allowed but flags
  `out_of_sync = true`** on that task (AC4 — manual override, not an
  auto-correction). A task with multiple predecessor edges (possibly of
  different types) is out-of-sync if *any* edge is violated.
- **A predecessor's `start_date` or `due_date` changes, forward
  propagation to successors** (AC1–2): for every outgoing edge whose
  constraint is now violated, the successor's *constrained field* shifts
  forward by the same delta (duration preserved — the other field moves
  with it), and that shift is *not* flagged out-of-sync — it's the
  system doing its job, not an override. Because FS/SS constrain the
  successor's `start_date` while FF/SF constrain its `due_date`, a
  single predecessor change is checked against all outgoing edges
  regardless of type — an SS edge can fire off a `start_date` change the
  same way an FS edge fires off a `due_date` change.

Cascade algorithm (used by both `update_task`'s forward-propagation step
and `preview_cascade`):

```
fn cascade(changed: TaskId, graph) -> Vec<Task> {
    let mut touched = HashMap::new();       // TaskId -> Task, dedup + latest value
    let mut queue = VecDeque::from([changed]);
    while let Some(current) = queue.pop_front() {
        let current_task = touched.get(&current).unwrap_or_else(|| graph.task(current));
        for (successor, edge) in graph.successor_edges_of(current) {  // (task, DependencyType)
            let succ_task = touched.get(&successor.id).unwrap_or(&successor);
            if !constraint_ok(edge, current_task, succ_task) {
                let anchor = anchor_date(edge, current_task);
                let delta = anchor - constrained_field(edge, succ_task);
                let shifted = succ_task.shift_by(delta);  // preserves duration; shifts both start and due
                touched.insert(successor.id, shifted);
                queue.push_back(successor.id);            // re-examine its own successors
            }
        }
    }
    touched.into_values().collect()
}
```

Because the dependency graph is acyclic by construction (§2 rejects
cycles at edge-add time, independent of edge type), this queue drains in
finite steps — a task can be re-pushed if a later relaxation shifts it
further out (multiple incoming edges), but each push strictly increases
its constrained field, so the loop terminates. `preview_cascade` runs
this against an in-memory copy of the affected subgraph and returns the
result **without** calling `Store::put_task` — same computation, no
commit, satisfying [#61](https://github.com/sleb/bala/issues/61) AC3's "preview before committing".

`update_task` calls `cascade` whenever `start_date` or `due_date` moves,
then persists `[changed_task] + touched` inside one `Store::transaction`,
and returns the full `Vec<Task>` so a caller can refresh every affected
view without re-querying (HLD §Interfaces guarantee).

### 4. Progress rollup ([#39](https://github.com/sleb/bala/issues/39) AC1–5)

Computed on read inside `get_tree`, never stored, never computed by a
caller (HLD guarantee). A task's `progress` is derived from its **direct**
children's own `status` flag only — never from a child's own (separately
computed) `progress`, and never by looking past direct children to
grandchildren:

```
fn direct_children_progress(children, own_status) -> f32 {
    if children.is_empty() {
        return match own_status { Complete => 1.0, Incomplete => 0.0 };  // AC5
    }
    children.iter()
        .map(|c| match c.status { Complete => 1.0, Incomplete => 0.0 })
        .sum::<f32>() / children.len() as f32
}
```

This is the corrected formula: averaging children's own `progress`
(rather than their `status`) would leak grandchildren's completion two
levels up, contradicting AC2. For example, a task with two direct
children, each itself `Complete` but each carrying ten incomplete
grandchildren of its own, rolls up to `1.0` (2 of 2 direct children
complete) — the grandchildren never factor in, because only the direct
children's own `status` is consulted, not their `progress`.

A task's own `status` is independent of its rollup number once it has
children (AC4) — `progress` and `status` are reported as two separate
fields on `Task`, never conflated. Because the formula only ever reads
each direct child's `status` (not its `progress`), there is no recursion
and nothing to memoize: computing one task's `progress` is a single flat
lookup of its direct children's `status`, independent of traversal order
or of any other task's rollup. `get_tree` computes this once per result
task, each a `list_child_edges` + `get_task`-per-child lookup — O(n + e)
for the whole tree per call (n result tasks, e parent-child edges walked
across all of them), not O(n) per task. With multiple parents a task can
have more than one incoming edge, so e can be as large as O(n²) in the
worst case — this is still one lookup per edge, not a repeated per-task
cost, but it is not O(n) outright.

With multiple parents, "children" is the reverse lookup — every task
whose `parent_ids` contains this task's id — and a task shared by two
parents contributes its `status` **independently to each parent's own
average**; there's no splitting, normalization, or memoization concern
across parents, since this is a per-parent lookup of `list_child_edges`
and each direct child's `status`, not a graph traversal (a Complete
shared child counts as a full `1.0` toward both Goal A's and Goal B's
rollup, computed separately for each).

### 5. `complete_task` ([#13](https://github.com/sleb/bala/issues/13) AC2, resolved per §Context)

```
fn complete_task(id, cascade) -> Result<Vec<Task>, CoreError> {
    let incomplete = incomplete_descendants(id);  // every depth, not just direct children
    if !incomplete.is_empty() && !cascade {
        return Err(CoreError::IncompleteChildren { task: id, incomplete });
    }
    // cascade == true: complete the whole subtree; cascade == false with
    // no incomplete descendants: complete just this task.
    mark_complete(id, recursive: cascade)
}
```

`incomplete_descendants` walks the whole subtree, not just direct
children, so `IncompleteChildren.incomplete` always names exactly the set
`cascade: true` would go on to complete — a caller rendering a
confirmation prompt from this error (e.g. the TUI's cascade-confirm,
[#26](https://github.com/sleb/bala/issues/26) AC3) gets an accurate count without re-walking the hierarchy
itself.

Returns every task actually completed (one, or the whole touched
subtree) — again so a caller refreshes views from the return value
rather than re-querying.

`reopen_task` is the reverse transition, Complete → Incomplete, added
when the TUI's completion toggle needed a way back.

## Storage Boundary

Not a network contract (HLD §4) — a Rust trait so `bala-core` is
testable against an in-memory fake today and the Data Store LLD's
embedded engine is just another implementation. Per HLD, hierarchy
(`parent_ids`) and dependency edges are related-but-independent graphs
stored distinctly, so the trait doesn't fold edges into the task row.
Multiple parents mean hierarchy is now an edge set rather than a single
FK column, so it gets the same edge-table shape dependencies already
have:

```rust
pub trait Store {
    fn transaction<T>(
        &self,
        f: impl FnOnce(&mut dyn StoreTx) -> Result<T, StoreError>,
    ) -> Result<T, StoreError>;
}

pub trait StoreTx {
    fn get_user(&mut self, id: UserId) -> Result<Option<User>, StoreError>;
    fn put_user(&mut self, user: &User) -> Result<(), StoreError>;
    fn list_users(&mut self) -> Result<Vec<User>, StoreError>;

    fn get_task(&mut self, id: TaskId) -> Result<Option<Task>, StoreError>;
    fn put_task(&mut self, task: &Task) -> Result<(), StoreError>;
    fn list_tasks(&mut self, filter: &TreeFilter) -> Result<Vec<Task>, StoreError>;

    fn list_parent_edges(&mut self, id: TaskId) -> Result<Vec<TaskId>, StoreError>;
    /// `None` = the top-level tasks (children of the NULL parent), in position order.
    fn list_child_edges(&mut self, parent: Option<TaskId>) -> Result<Vec<TaskId>, StoreError>;
    /// Every (parent, child) edge grouped by parent, position order within each
    /// (`None` = top level), tombstones included. One call, so whole-hierarchy
    /// reads (`Core::sibling_order`) avoid a `list_child_edges` per parent.
    fn list_all_child_edges(&mut self) -> Result<Vec<(Option<TaskId>, TaskId)>, StoreError>;
    /// `Placement::End` appends; `Placement::After(sibling)` inserts right after
    /// `sibling` in each newly added parent's children, falling back to `End` for
    /// a parent that does not contain `sibling` (or if `sibling == child`).
    /// Retained parents keep their position.
    fn replace_parent_edges(&mut self, child: TaskId, parents: &Parents, placement: Placement) -> Result<(), StoreError>;
    /// Swaps the positions of `a` and `b` among `parent`'s children only (other
    /// parents' positions untouched); no-op if either is not a child of `parent`.
    fn swap_child_positions(&mut self, parent: Option<TaskId>, a: TaskId, b: TaskId) -> Result<(), StoreError>;

    fn list_dependency_edges(&mut self, id: TaskId) -> Result<Vec<Dependency>, StoreError>;
    fn list_successor_edges(&mut self, id: TaskId) -> Result<Vec<TaskId>, StoreError>;
    fn add_dependency_edge(&mut self, predecessor: TaskId, successor: TaskId, dep_type: DependencyType) -> Result<(), StoreError>;
    fn remove_dependency_edge(&mut self, predecessor: TaskId, successor: TaskId) -> Result<(), StoreError>;

    fn get_task_types(&mut self) -> Result<Vec<TaskType>, StoreError>;
    fn put_task_type(&mut self, t: &TaskType) -> Result<(), StoreError>;
}
```

`list_child_edges`/`list_successor_edges` are the reverse of
`list_parent_edges`/`list_dependency_edges` — resolving Data Store LLD's
§Open Questions item 1 as named trait methods rather than something
`Core` reconstructs in memory from a bulk `list_tasks`. §Algorithm 3's
cascade (`graph.successor_edges_of(current)`) calls `list_successor_edges`
per task walked, and §Algorithm 4's rollup ("children" = reverse lookup
of `parent_ids`) calls `list_child_edges` per task in its post-order
traversal — both were already relying on this direction existing, just
without a named method to call.

Every `Core` method that touches more than one task (cascade, subtree
delete/complete) wraps its writes in one `Store::transaction` call, so a
cascade either commits as a whole or not at all — the Data Store LLD's
job is to make `transaction` atomic for whatever engine it picks, not to
redesign this boundary.

**Not addressed here (matches HLD's open item):** multiple concurrent
writers. `Core` assumes single-writer-at-a-time, consistent with a local
embedded store; multi-client concurrent editing is still an open question
flagged at the HLD level, not solved by adding locking here.

## Testing Strategy

- `hierarchy`, `scheduling`, and `rollup` are unit-tested against an
  in-memory fake `Store` — no I/O, fast, and exercises exactly the
  algorithms in §Algorithms.
- Name tests for behavior, not method name, per rust-best-practices:
  e.g. `add_dependency_should_reject_cycle_through_transitive_predecessor`,
  `complete_task_should_block_when_children_incomplete_and_cascade_false`,
  `set_parents_should_reject_whole_call_when_one_candidate_creates_cycle`,
  `delete_subtree_should_keep_child_reachable_through_other_parent`.
- Cascade tests per `DependencyType`: one predecessor/successor pair for
  each of FS/SS/FF/SF confirming the right field pair (start↔start,
  due↔start, due↔due, start↔due) is checked and shifted; plus a mixed
  case where the same predecessor has both an FS and an SS successor to
  confirm each is evaluated by its own edge's rule independently.
- Property-style tests for cascade: random small DAGs (with a random mix
  of dependency types), assert the post-cascade graph satisfies each
  edge's own type-specific constraint and that `preview_cascade` and
  `update_task` agree on the touched set before the latter commits.
- Multi-parent rollup: a task shared by two parents contributes its own
  `status` to each parent's average independently, computed per-parent
  from that parent's own `list_child_edges`; since this is a flat
  per-parent lookup rather than a graph traversal, there's no
  memoization concern to test — each parent's `get_tree` result should
  simply reflect only its own direct children.

## Deferred to Other LLDs

- **Data Store LLD:** which embedded engine implements `Store`/`StoreTx`,
  schema, indexing for tree + dependency queries at 200+ tasks. (Its own
  two open questions back to this LLD — reverse-edge trait methods and
  `TreeFilter`'s fields — are now resolved above: `list_child_edges`/
  `list_successor_edges` and §Data Model's `TreeFilter`.)
- **CLI/TUI Client LLD:** how `preview_cascade`'s result is rendered as a
  confirmation prompt ([#61](https://github.com/sleb/bala/issues/61) AC3) and how `IncompleteChildren`/
  `CircularHierarchy`/etc. map to on-screen messages.
- **Web API LLD:** near-mechanical mapping of this method contract onto
  routes; `CoreError` variants map onto HTTP status + JSON error body.

## Consequences

- The three method-shape decisions from §Context (soft-delete,
  block-then-cascade completion, `Field<T>` patches) are now load-bearing
  on the public API — changing any later is a breaking change to every
  caller, including the future Web API.
- `Core<S: Store>` being generic rather than trait-object-based means one
  monomorphized build per concrete store; fine today (one store type),
  worth revisiting only if a second concrete `Store` impl (e.g. a test
  double shipped in the same binary as production) ever needs to coexist
  at runtime rather than at compile time.
- `preview_cascade` and the committing path in `update_task` share the
  same `cascade()` function by construction — there's no way for preview
  to drift from what actually commits, which is the whole point of
  [#61](https://github.com/sleb/bala/issues/61) AC3.
- Multiple parents turn the hierarchy into a DAG, which is now load-bearing
  on §1's invariant check, §2's ancestor/descendant walk, §4's rollup,
  and delete cascade semantics (§Method Contract) — a future move back to
  strict single-parent would be a simplification, not free, since callers
  (including the Web API) would already be built against `Vec<TaskId>`.
- Typed dependencies are similarly load-bearing on §3's cascade algorithm:
  starting with FS-only usage in practice doesn't defer any of this
  complexity, since the constraint/anchor abstraction had to exist from
  the start to keep FS a special case of the general rule rather than a
  hardcoded path SS/FF/SF would later have to be retrofitted around.

## Action Items
1. [ ] Scaffold `bala-core` crate with the module layout in §Decision
2. [ ] Implement `model`, `CoreError`, `Field<T>`/`TaskPatch` (no logic yet)
3. [ ] Implement `hierarchy` module + tests (§Algorithm 1)
4. [ ] Implement `scheduling` module + tests (§Algorithms 2–3), including `preview_cascade`
5. [ ] Implement `rollup` module + tests (§Algorithm 4)
6. [ ] Implement `complete_task` + `delete_task`/`restore_task` (§Algorithm 5, soft-delete)
7. [ ] Define in-memory fake `Store` for tests; hand the `Store`/`StoreTx` traits to the Data Store LLD
