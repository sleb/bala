# Bala — Core Library Implementation Plan

**Design:** [../core-library.md](../core-library.md) (single Core Library
design per HLD.md — CRUD, hierarchy, dependencies, scheduling, and rollup
stay one component; this plan sequences its build, it does not re-split
the design itself).
**Related:** [STORIES.md](../STORIES.md) — sequenced by epic, Epic 1
first per that file's own build order ("Epic 1 → Epic 2 → Epic 3 → Epic
4, since the Gantt chart and dependency enforcement both depend on core
task CRUD and hierarchy existing first").

Slice order: **Epic 1 (Core Task Management) → Epic 2 (Nested Hierarchy)
→ Epic 3 (Task Dependencies)**. Epic 4 (Gantt) is CLI/TUI Client LLD
territory, not this crate. Within each slice, every design.md Testing
Strategy test is sequenced immediately before the implementation step it
drives, per Bala's strict-TDD discipline.

Epic 1 needs a minimal slice of hierarchy (parent-edge bookkeeping for
`create_task`'s `parent_ids` and `delete_task`'s `DeleteMode`) ahead of
Epic 2's own full `set_parents` — design.md's §Algorithm 1 already
specifies one cycle-check used by both call sites, so step 9 below
implements it once and step 29 (Epic 2) reuses it, it doesn't reimplement
it.

## Setup

- [ ] 1. Scaffold `bala-core` crate: `Cargo.toml` + module layout
      (`facade`, `hierarchy`, `scheduling`, `rollup`, `types`, `model`,
      `store`) per design.md §Decision — empty modules, no logic yet.
- [ ] 2. Implement an in-memory fake `Store`/`StoreTx` (`store::fake::FakeStore`)
      per design.md §Storage Boundary, so every test below runs with no I/O.

## Epic 1 — Core Task Management (Stories 1.1–1.4)

**Model & errors**
- [ ] 3. Implement `model`: `TaskId`/`UserId` newtypes, `Task`, `TaskStatus`,
      `NewTask`, `Field<T>`, `TaskPatch` (design.md §Data Model) — types only.
- [ ] 4. Implement `CoreError` (design.md §Error Taxonomy): `NotFound`,
      `EmptyTitle`, `InvalidDateRange`, `CircularHierarchy`,
      `IncompleteChildren`, `Store(#[from] StoreError)` — the other
      variants (`DependsOnRelative`, `CircularDependency`,
      `UnknownTaskType`) are added in their own Epic 2/3 steps below.

**`create_task` (Story 1.1)**
- [ ] 5. Write `create_task_should_reject_empty_title` (AC4).
- [ ] 6. Write `create_task_should_persist_and_return_task_with_id_and_created_at` (AC1, AC5).
- [ ] 7. Write `create_task_should_default_to_top_level_when_no_parent_given` (AC6).
- [ ] 8. Write `create_task_should_reject_parent_that_would_make_task_its_own_ancestor`
      (design.md §Algorithm 1, exercised via the creation path).
- [ ] 9. Implement `Core::create_task`, plus `hierarchy::check_no_cycle`
      (§Algorithm 1's shared cycle-check helper — creation call site only;
      `set_parents` reuses it unchanged in step 29).

**`update_task` (Story 1.2)**
- [ ] 10. Write `update_task_should_apply_only_fields_marked_set_and_leave_keep_fields_untouched`.
- [ ] 11. Write `update_task_should_clear_optional_field_when_patch_is_clear`.
- [ ] 12. Write `update_task_should_reject_due_date_before_start_date` (AC4).
- [ ] 13. Write `update_task_should_record_updated_at_timestamp` (AC3).
- [ ] 14. Implement `Core::update_task` — validation + persist only; the
       forward-cascade hook (§Algorithm 3) is wired in at step 44 once
       dependencies exist, so this step is a plain field update, matching
       Story 1.2's scope (no dependency ACs).

**`delete_task` / `restore_task` (Story 1.3)**
- [ ] 15. Write `delete_task_should_soft_delete_leaf_task` (AC4, AC5).
- [ ] 16. Write `delete_subtree_should_tombstone_children_left_with_no_parents` (AC2).
- [ ] 17. Write `delete_subtree_should_keep_child_reachable_through_other_parent` (AC2).
- [ ] 18. Write `delete_promote_children_should_reattach_to_deleted_tasks_parents` (AC2).
- [ ] 19. Write `restore_task_should_clear_deleted_at`.
- [ ] 20. Implement `hierarchy::remove_parent_edges` / `promote_children`
       helpers (parent-edge bookkeeping only — no cycle check needed, per
       design.md §Method Contract's `remove_dependency` note that removal
       can't introduce a cycle, and the same holds for edge removal here).
- [ ] 21. Implement `Core::delete_task` (`DeleteMode::Subtree | PromoteChildren`)
       and `Core::restore_task`.

**`complete_task` (Story 1.4)**
- [ ] 22. Write `complete_task_should_block_when_children_incomplete_and_cascade_false` (AC2).
- [ ] 23. Write `complete_task_should_cascade_complete_whole_subtree_when_cascade_true` (AC2, AC4).
- [ ] 24. Write `complete_task_should_record_completed_at_timestamp` (AC5).
- [ ] 25. Implement `Core::complete_task` (design.md §Algorithm 5).

- [ ] 26. Epic 1 checkpoint: confirm every Story 1.1–1.4 AC is covered by
       a test above; `create_task`/`update_task`/`delete_task`/
       `restore_task`/`complete_task` are the full surface the CLI/TUI
       Client LLD needs to build Epic 1's screens against.

## Epic 2 — Nested Task Hierarchy (Stories 2.1, 2.3, 2.4 — 2.2 is CLI/TUI-only)

- [ ] 27. Write `set_parents_should_reject_whole_call_when_one_candidate_creates_cycle` (Story 2.1 AC5).
- [ ] 28. Write `set_parents_should_apply_atomically_no_partial_reparenting` (Story 2.1 AC4).
- [ ] 29. Implement `hierarchy::set_parents` (design.md §Algorithm 1 in
       full, reusing `check_no_cycle` from step 9).
- [ ] 30. Write `rollup_should_contribute_full_progress_to_each_parent_independently`
       (Story 2.3 AC1–3, multi-parent case).
- [ ] 31. Write `rollup_should_compute_shared_descendant_once_not_once_per_path` (memoization).
- [ ] 32. Write `rollup_should_use_own_status_when_task_has_no_children` (Story 2.3 AC5).
- [ ] 33. Implement `rollup` module (design.md §Algorithm 4) and wire
       `Core::get_tree` to compute `progress` on read.
- [ ] 34. Write `upsert_task_type_should_add_new_type_and_list_task_types_should_return_it` (Story 2.4 AC1, AC5).
- [ ] 35. Implement `types` module: `Core::list_task_types` / `Core::upsert_task_type`,
       add `CoreError::UnknownTaskType`.

## Epic 3 — Task Dependencies (Stories 3.1–3.3)

- [ ] 36. Write `add_dependency_should_reject_self_dependency` (Story 3.1 AC2).
- [ ] 37. Write `add_dependency_should_reject_ancestor_or_descendant_as_predecessor` (Story 3.1 AC2).
- [ ] 38. Write `add_dependency_should_reject_cycle_through_transitive_predecessor` (Story 3.1 AC3).
- [ ] 39. Write `add_dependency_should_replace_existing_edge_of_different_type`.
- [ ] 40. Implement `scheduling::add_dependency` / `remove_dependency`
       (design.md §Algorithm 2), add `CoreError::DependsOnRelative` /
       `CircularDependency`.
- [ ] 41. Write per-type cascade tests: `cascade_should_shift_successor_start_on_finish_to_start_violation`,
       `cascade_should_shift_successor_start_on_start_to_start_violation`,
       `cascade_should_shift_successor_due_on_finish_to_finish_violation`,
       `cascade_should_shift_successor_due_on_start_to_finish_violation`
       (Story 3.2 AC1).
- [ ] 42. Write `cascade_should_evaluate_mixed_dependency_types_independently_from_same_predecessor`.
- [ ] 43. Write `cascade_should_satisfy_every_edges_constraint_on_random_dags`
       (property test, Story 3.2 AC2).
- [ ] 44. Write `preview_cascade_and_update_task_should_agree_on_touched_set_before_commit`
       (Story 3.2 AC3).
- [ ] 45. Implement `scheduling::cascade` (design.md §Algorithm 3) and wire
       it into `Core::update_task`'s forward-propagation step and
       `Core::preview_cascade`.
- [ ] 46. Write `update_task_should_flag_out_of_sync_when_manual_edit_violates_predecessor_constraint`
       (Story 3.2 AC4).
- [ ] 47. Implement out-of-sync flagging on direct edits (§Algorithm 3,
       first trigger) — completes `Core::update_task`.
- [ ] 48. Write `complete_task_should_unblock_dependents_for_status_purposes_without_touching_dates`
       (Story 3.2 AC5, Story 3.3).

## Finish line

- [ ] 49. Full crate review: every design.md §Method Contract signature is
       implemented; hand the finished `Store`/`StoreTx` traits to the
       Data Store LLD unchanged (design.md §Storage Boundary).
