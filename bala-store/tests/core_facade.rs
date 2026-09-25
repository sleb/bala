//! Re-runs `bala-core`'s `Core` facade behavioral suite
//! (`bala-core/src/facade.rs`'s `tests` module) against `SqliteStore`
//! instead of `InMemoryStore`, to prove `Core::create_task`/`get_tree`/
//! `list_task_types`/`upsert_task_type` behave identically regardless of
//! which `Store` backs them.
//!
//! Test bodies and assertions are ported verbatim from `bala-core`'s
//! suite — only the store construction changes (`SqliteStore::open_in_memory()`
//! instead of `InMemoryStore::default()`). This file does not re-test
//! `SqliteStore`'s own storage primitives (see `bala-store/src/store.rs`'s
//! `tests` module for those); it is scoped strictly to the `Core` facade's
//! behavior over a SQLite-backed store.

use bala_core::{Core, CoreError, NewTask, TaskId, TaskStatus, TaskType, TreeFilter};
use bala_store::SqliteStore;
use chrono::NaiveDate;

fn new_core() -> Core<SqliteStore> {
    Core::new(SqliteStore::open_in_memory().unwrap()).unwrap()
}

fn minimal_new_task(title: &str) -> NewTask {
    NewTask {
        title: title.to_owned(),
        description: None,
        parent_ids: Vec::new(),
        type_key: None,
        start_date: None,
        due_date: None,
        assignee_id: None,
    }
}

#[test]
fn create_task_should_reject_empty_title() {
    let mut core = new_core();

    let result = core.create_task(minimal_new_task(""));

    assert!(matches!(result, Err(CoreError::EmptyTitle)));
}

#[test]
fn create_task_should_reject_whitespace_only_title() {
    let mut core = new_core();

    let result = core.create_task(minimal_new_task("   \t  "));

    assert!(matches!(result, Err(CoreError::EmptyTitle)));
}

#[test]
fn create_task_should_assign_unique_id_and_created_at() {
    let mut core = new_core();

    let a = core.create_task(minimal_new_task("Task A")).unwrap();
    let b = core.create_task(minimal_new_task("Task B")).unwrap();

    assert_ne!(a.id, b.id);
    assert_eq!(a.created_at, a.updated_at);
    assert_eq!(b.created_at, b.updated_at);
}

#[test]
fn create_task_should_default_to_top_level_when_no_parent_given() {
    let mut core = new_core();

    let task = core.create_task(minimal_new_task("Top level")).unwrap();

    assert!(task.parent_ids.is_empty());
}

#[test]
fn create_task_should_attach_to_given_parent_when_parent_ids_provided() {
    let mut core = new_core();
    let parent = core.create_task(minimal_new_task("Parent")).unwrap();

    let child = core
        .create_task(NewTask {
            parent_ids: vec![parent.id],
            ..minimal_new_task("Child")
        })
        .unwrap();

    assert_eq!(child.parent_ids, vec![parent.id]);
}

#[test]
fn create_task_should_reject_when_given_parent_does_not_exist() {
    let mut core = new_core();
    let missing_parent = TaskId::new();

    let result = core.create_task(NewTask {
        parent_ids: vec![missing_parent],
        ..minimal_new_task("Orphan")
    });

    assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing_parent));
}

#[test]
fn create_task_should_default_type_key_to_task_when_unset() {
    let mut core = new_core();

    let task = core.create_task(minimal_new_task("Untyped")).unwrap();

    assert_eq!(task.type_key, "task");
}

#[test]
fn create_task_should_reject_unknown_type_key() {
    let mut core = new_core();

    let result = core.create_task(NewTask {
        type_key: Some("bogus".to_owned()),
        ..minimal_new_task("Mistyped")
    });

    assert!(matches!(result, Err(CoreError::UnknownTaskType(key)) if key == "bogus"));
}

#[test]
fn create_task_should_store_optional_description_and_dates() {
    let mut core = new_core();
    let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
    let due = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();

    let task = core
        .create_task(NewTask {
            description: Some("Details".to_owned()),
            start_date: Some(start),
            due_date: Some(due),
            ..minimal_new_task("With details")
        })
        .unwrap();

    assert_eq!(task.description.as_deref(), Some("Details"));
    assert_eq!(task.start_date, Some(start));
    assert_eq!(task.due_date, Some(due));
}

#[test]
fn create_task_should_reject_due_date_before_start_date() {
    let mut core = new_core();
    let start = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
    let due = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

    let result = core.create_task(NewTask {
        start_date: Some(start),
        due_date: Some(due),
        ..minimal_new_task("Backwards dates")
    });

    assert!(matches!(
        result,
        Err(CoreError::InvalidDateRange { start: s, due: d }) if s == start && d == due
    ));
}

#[test]
fn create_task_should_record_an_edge_for_each_given_parent() {
    // Attaching a new task under several parents in one call succeeds
    // and records one edge per parent, in the given order.
    let mut core = new_core();
    let parent_a = core.create_task(minimal_new_task("Parent A")).unwrap();
    let parent_b = core.create_task(minimal_new_task("Parent B")).unwrap();

    let child = core
        .create_task(NewTask {
            parent_ids: vec![parent_a.id, parent_b.id],
            ..minimal_new_task("Multi-parent child")
        })
        .unwrap();

    assert_eq!(child.parent_ids, vec![parent_a.id, parent_b.id]);
}

#[test]
fn get_tree_should_include_a_just_created_task() {
    let mut core = new_core();

    let created = core.create_task(minimal_new_task("Findable")).unwrap();
    let tree = core.get_tree(TreeFilter::default()).unwrap();

    assert!(tree.contains(&created));
}

#[test]
fn new_should_seed_default_task_type() {
    let core = new_core();

    let types = core.list_task_types().unwrap();

    assert!(types.iter().any(|t| t.key == "task"));
}

#[test]
fn new_is_idempotent_about_seeding_the_default_task_type() {
    let store = SqliteStore::open_in_memory().unwrap();
    let core_a = Core::new(store).unwrap();
    let types_after_first = core_a.list_task_types().unwrap();
    assert_eq!(
        types_after_first.iter().filter(|t| t.key == "task").count(),
        1
    );

    // Re-seeding over a store that already has the default leaves it
    // as a single entry, not a duplicate. `SqliteStore::open_in_memory`
    // already seeds the default type via migrations, so this also
    // exercises `Core::new`'s idempotency against a store whose default
    // type predates `Core::new` even being called.
    let store_with_default = SqliteStore::open_in_memory().unwrap();
    let core_b = Core::new(store_with_default).unwrap();
    let types_after_second = core_b.list_task_types().unwrap();
    assert_eq!(
        types_after_second
            .iter()
            .filter(|t| t.key == "task")
            .count(),
        1
    );
}

#[test]
fn list_task_types_and_upsert_task_type_round_trip() {
    let mut core = new_core();
    let goal = TaskType {
        key: "goal".to_owned(),
        label: "Goal".to_owned(),
        color: None,
        sort_order: 1,
    };

    let upserted = core.upsert_task_type(goal.clone()).unwrap();
    let types = core.list_task_types().unwrap();

    assert_eq!(upserted, goal);
    assert!(types.contains(&goal));
}

#[test]
fn create_task_uses_status_incomplete() {
    let mut core = new_core();

    let task = core.create_task(minimal_new_task("New")).unwrap();

    assert_eq!(task.status, TaskStatus::Incomplete);
}
