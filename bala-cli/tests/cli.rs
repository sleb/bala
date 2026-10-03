//! Integration tests for the `bala` binary, driven end-to-end via
//! `assert_cmd`. Each test points the CLI at its own temp-file SQLite
//! database via the `--db-path` flag, so tests never touch the real
//! default OS data dir and never interfere with each other.

use std::time::Duration;

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn bala_cmd(db_path: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("bala").unwrap();
    cmd.arg("--db-path").arg(db_path);
    cmd
}

#[test]
fn task_add_with_title_only_should_succeed_and_print_new_task_id() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    bala_cmd(&db_path)
        .args(["task", "add", "--title", "Write plan"])
        .assert()
        .success()
        .stdout(contains(
            // A UUID's hex digits and dashes are enough to confirm an id
            // was printed without pinning the exact value.
            "-",
        ));
}

#[test]
fn task_add_with_empty_title_should_fail_with_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    bala_cmd(&db_path)
        .args(["task", "add", "--title", ""])
        .assert()
        .failure();
}

#[test]
fn task_add_then_task_ls_should_show_the_new_task() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    bala_cmd(&db_path)
        .args(["task", "add", "--title", "Write plan"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("Write plan"));
}

#[test]
fn task_add_with_parent_should_nest_under_it_in_ls_output() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_output = bala_cmd(&db_path)
        .args(["task", "add", "--title", "Parent task"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parent_id = String::from_utf8(parent_output).unwrap().trim().to_owned();

    bala_cmd(&db_path)
        .args([
            "task",
            "add",
            "--title",
            "Child task",
            "--parent",
            &parent_id,
        ])
        .assert()
        .success();

    let ls_output = bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ls_text = String::from_utf8(ls_output).unwrap();

    let parent_line = ls_text
        .lines()
        .find(|line| line.contains("Parent task"))
        .expect("parent line present");
    let child_line = ls_text
        .lines()
        .find(|line| line.contains("Child task"))
        .expect("child line present");

    let parent_indent = parent_line.len() - parent_line.trim_start().len();
    let child_indent = child_line.len() - child_line.trim_start().len();
    assert!(
        child_indent > parent_indent,
        "expected child line to be indented more than its parent: parent={parent_line:?} child={child_line:?}"
    );
}

#[test]
fn task_add_with_repeated_parent_should_fail_usage_and_create_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a_id = add_task(&db_path, &["--title", "Goal A"]);
    let b_id = add_task(&db_path, &["--title", "Goal B"]);

    bala_cmd(&db_path)
        .args([
            "task",
            "add",
            "--title",
            "Shared child",
            "--parent",
            &a_id,
            "--parent",
            &b_id,
        ])
        .assert()
        .code(2)
        .stdout("")
        .stderr(contains("--parent"));

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("Shared child").not());
}

#[test]
fn user_add_then_ls_shows_created_user() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    bala_cmd(&db_path)
        .args(["user", "add", "--name", "Ada"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["user", "ls"])
        .assert()
        .success()
        .stdout(contains("Ada"));
}

#[test]
fn task_add_with_assignee_sets_assignee() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let user_output = bala_cmd(&db_path)
        .args(["user", "add", "--name", "Ada"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let user_id = String::from_utf8(user_output).unwrap().trim().to_owned();

    bala_cmd(&db_path)
        .args(["task", "add", "--title", "Ship it", "--assignee", &user_id])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(
            contains("Ship it")
                .and(contains("Ada"))
                .and(contains(&user_id).not()),
        );
}

#[test]
fn task_add_with_unknown_assignee_fails_with_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let unknown_user_id = uuid::Uuid::new_v4().to_string();

    bala_cmd(&db_path)
        .args([
            "task",
            "add",
            "--title",
            "Ship it",
            "--assignee",
            &unknown_user_id,
        ])
        .assert()
        .failure()
        .stderr(contains("unknown user"));
}

fn add_task(db_path: &std::path::Path, args: &[&str]) -> String {
    let mut full_args = vec!["task", "add"];
    full_args.extend_from_slice(args);
    let output = bala_cmd(db_path)
        .args(full_args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).unwrap().trim().to_owned()
}

#[test]
fn task_edit_should_update_title_and_show_in_ls() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Old title"]);

    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--title", "New title"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("New title").and(contains("Old title").not()));
}

#[test]
fn task_edit_should_clear_description() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(
        &db_path,
        &["--title", "Task with desc", "--description", "Some desc"],
    );

    // `task ls` doesn't currently surface description at all, so there's no
    // visible-output assertion available here — the observable surface is
    // just that the edit itself succeeds.
    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--clear-description"])
        .assert()
        .success();
}

#[test]
fn task_edit_with_empty_title_should_fail_with_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Has a title"]);

    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--title", ""])
        .assert()
        .failure();
}

#[test]
fn task_edit_with_unknown_task_id_should_fail() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let unknown_task_id = uuid::Uuid::new_v4().to_string();

    bala_cmd(&db_path)
        .args([
            "task",
            "edit",
            &unknown_task_id,
            "--title",
            "Doesn't matter",
        ])
        .assert()
        .failure();
}

#[test]
fn task_edit_should_set_and_clear_assignee() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let user_id = String::from_utf8(
        bala_cmd(&db_path)
            .args(["user", "add", "--name", "Ada"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_owned();

    let task_id = add_task(&db_path, &["--title", "Ship it"]);

    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--assignee", &user_id])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("Ada"));

    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--clear-assignee"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("Ada").not());
}

#[test]
fn task_edit_with_conflicting_value_and_clear_flags_should_fail_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Ship it"]);

    bala_cmd(&db_path)
        .args([
            "task",
            "edit",
            &task_id,
            "--description",
            "X",
            "--clear-description",
        ])
        .assert()
        .failure();
}

#[test]
fn task_delete_leaf_task_with_yes_flag_should_succeed_and_remove_from_ls() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Leaf task"]);

    bala_cmd(&db_path)
        .args(["task", "delete", &task_id, "--yes"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("Leaf task").not());
}

#[test]
fn task_delete_without_yes_and_declined_confirmation_should_not_delete() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Leaf task"]);

    bala_cmd(&db_path)
        .args(["task", "delete", &task_id])
        .write_stdin("n\n")
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("Leaf task"));
}

#[test]
fn task_delete_with_unknown_task_id_should_fail() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let unknown_task_id = uuid::Uuid::new_v4().to_string();

    bala_cmd(&db_path)
        .args(["task", "delete", &unknown_task_id, "--yes"])
        .assert()
        .failure();
}

#[test]
fn task_delete_task_with_subtasks_and_no_mode_flag_should_fail_with_warning() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_id = add_task(&db_path, &["--title", "Parent task"]);
    add_task(&db_path, &["--title", "Child task", "--parent", &parent_id]);

    bala_cmd(&db_path)
        .args(["task", "delete", &parent_id, "--yes"])
        .assert()
        .failure()
        .stderr(contains("Child task"));
}

#[test]
fn task_delete_task_with_subtasks_and_cascade_should_remove_child_from_ls_too() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_id = add_task(&db_path, &["--title", "Parent task"]);
    add_task(&db_path, &["--title", "Child task", "--parent", &parent_id]);

    bala_cmd(&db_path)
        .args(["task", "delete", &parent_id, "--cascade", "--yes"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(
            contains("Parent task")
                .not()
                .and(contains("Child task").not()),
        );
}

/// Runs `bala task ls` and returns its stdout.
fn task_ls(db_path: &std::path::Path) -> String {
    let output = bala_cmd(db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).unwrap()
}

/// The `task ls` lines naming `title`.
fn lines_naming<'a>(ls_text: &'a str, title: &str) -> Vec<&'a str> {
    ls_text
        .lines()
        .filter(|line| line.contains(title))
        .collect()
}

/// The single `task ls` line for the task with this id.
fn ls_line(db_path: &std::path::Path, task_id: &str) -> String {
    let ls_text = task_ls(db_path);
    let lines = lines_naming(&ls_text, task_id);
    assert_eq!(lines.len(), 1, "{task_id} should be on one line: {ls_text}");
    lines[0].to_owned()
}

#[test]
fn task_add_with_duration_should_store_it() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Build", "--duration", "3"]);

    assert_eq!(
        ls_line(&db_path, &task_id),
        format!("[ ] {task_id} Build (type: task) (3d)")
    );
}

#[test]
fn task_edit_duration_should_replace_it() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let task_id = add_task(&db_path, &["--title", "Build", "--duration", "3"]);

    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--duration", "5"])
        .assert()
        .success()
        .stdout(contains("(5d)"));

    assert_eq!(
        ls_line(&db_path, &task_id),
        format!("[ ] {task_id} Build (type: task) (5d)")
    );
}

#[test]
fn task_edit_clear_duration_should_remove_it() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let task_id = add_task(&db_path, &["--title", "Build", "--duration", "3"]);

    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--clear-duration"])
        .assert()
        .success();

    assert_eq!(
        ls_line(&db_path, &task_id),
        format!("[ ] {task_id} Build (type: task)")
    );
}

#[test]
fn task_edit_with_negative_duration_should_fail_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let task_id = add_task(&db_path, &["--title", "Build", "--duration", "3"]);

    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--duration", "-1"])
        .assert()
        .code(2)
        .stderr(contains("invalid value '-1' for '--duration"));

    assert_eq!(
        ls_line(&db_path, &task_id),
        format!("[ ] {task_id} Build (type: task) (3d)")
    );
}

#[test]
fn task_edit_float_should_release_a_task() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let task_id = add_task(&db_path, &["--title", "Build", "--start", "2026-10-05"]);
    assert!(ls_line(&db_path, &task_id).ends_with(" 2026-10-05..? (fixed)"));

    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--float"])
        .assert()
        .success();

    assert_eq!(
        ls_line(&db_path, &task_id),
        format!("[ ] {task_id} Build (type: task) 2026-10-05..?")
    );
}

#[test]
fn task_edit_fix_should_fix_a_floating_task() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let task_id = add_task(&db_path, &["--title", "Build", "--start", "2026-10-05"]);
    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--float"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--fix"])
        .assert()
        .success();

    assert_eq!(
        ls_line(&db_path, &task_id),
        format!("[ ] {task_id} Build (type: task) 2026-10-05..? (fixed)")
    );
}

#[test]
fn task_edit_start_with_float_should_set_a_floating_date() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let task_id = add_task(&db_path, &["--title", "Build"]);

    bala_cmd(&db_path)
        .args([
            "task",
            "edit",
            &task_id,
            "--start",
            "2026-10-05",
            "--duration",
            "3",
            "--float",
        ])
        .assert()
        .success()
        .stdout(format!(
            "[ ] {task_id} Build (type: task) 2026-10-05..? (3d)\n"
        ));
}

#[test]
fn task_edit_fix_and_float_together_should_fail_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let task_id = add_task(&db_path, &["--title", "Build", "--start", "2026-10-05"]);

    bala_cmd(&db_path)
        .args(["task", "edit", &task_id, "--fix", "--float"])
        .assert()
        .code(2)
        .stderr(contains("cannot be used with"));

    assert!(ls_line(&db_path, &task_id).ends_with(" (fixed)"));
}

#[test]
fn task_ls_should_show_dates_duration_and_fixed_marker() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let user_id = String::from_utf8(
        bala_cmd(&db_path)
            .args(["user", "add", "--name", "Ada"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_owned();

    let both_id = add_task(
        &db_path,
        &[
            "--title",
            "Both dates",
            "--assignee",
            &user_id,
            "--start",
            "2026-10-05",
            "--due",
            "2026-10-08",
            "--duration",
            "3",
        ],
    );
    let due_only_id = add_task(&db_path, &["--title", "Due only", "--due", "2026-10-08"]);

    assert_eq!(
        ls_line(&db_path, &both_id),
        format!(
            "[ ] {both_id} Both dates (type: task) (assigned: Ada) 2026-10-05..2026-10-08 (3d) (fixed)"
        )
    );
    assert_eq!(
        ls_line(&db_path, &due_only_id),
        format!("[ ] {due_only_id} Due only (type: task) ?..2026-10-08 (fixed)")
    );
}

#[test]
fn task_ls_should_show_no_date_suffix_for_a_task_without_dates_or_duration() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Plain"]);

    assert_eq!(
        ls_line(&db_path, &task_id),
        format!("[ ] {task_id} Plain (type: task)")
    );
}

#[test]
fn task_delete_promote_children_should_move_children_to_top_level_without_a_grandparent() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_id = add_task(&db_path, &["--title", "Parent task"]);
    add_task(&db_path, &["--title", "Child one", "--parent", &parent_id]);
    add_task(&db_path, &["--title", "Child two", "--parent", &parent_id]);

    bala_cmd(&db_path)
        .args(["task", "delete", &parent_id, "--promote-children", "--yes"])
        .assert()
        .success();

    let ls_text = task_ls(&db_path);
    assert!(!ls_text.contains("Parent task"), "{ls_text}");
    for title in ["Child one", "Child two"] {
        let lines = lines_naming(&ls_text, title);
        assert_eq!(lines.len(), 1, "{title} listed once: {ls_text}");
        assert!(
            lines[0].starts_with('['),
            "{title} should be top level (unindented): {ls_text}"
        );
    }
}

#[test]
fn task_delete_promote_children_should_move_children_to_the_grandparent() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let grandparent_id = add_task(&db_path, &["--title", "Grandparent task"]);
    let parent_id = add_task(
        &db_path,
        &["--title", "Parent task", "--parent", &grandparent_id],
    );
    add_task(&db_path, &["--title", "Child one", "--parent", &parent_id]);
    add_task(&db_path, &["--title", "Child two", "--parent", &parent_id]);

    bala_cmd(&db_path)
        .args(["task", "delete", &parent_id, "--promote-children", "--yes"])
        .assert()
        .success();

    let ls_text = task_ls(&db_path);
    assert!(!ls_text.contains("Parent task"), "{ls_text}");
    for title in ["Child one", "Child two"] {
        let lines = lines_naming(&ls_text, title);
        assert_eq!(lines.len(), 1, "{title} listed once: {ls_text}");
        assert!(
            lines[0].starts_with("  ["),
            "{title} should still be nested: {ls_text}"
        );
    }

    // `task ls` indents every subtask alike, so the listing alone doesn't
    // say whose children they now are: a mode-less delete of the
    // grandparent refuses and names its direct subtasks.
    bala_cmd(&db_path)
        .args(["task", "delete", &grandparent_id, "--yes"])
        .assert()
        .failure()
        .stderr(contains("Child one").and(contains("Child two")));
}

#[test]
fn task_delete_promote_children_should_print_only_the_deleted_task_id() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_id = add_task(&db_path, &["--title", "Parent task"]);
    add_task(&db_path, &["--title", "Child task", "--parent", &parent_id]);

    let output = bala_cmd(&db_path)
        .args(["task", "delete", &parent_id, "--promote-children", "--yes"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(String::from_utf8(output).unwrap(), format!("{parent_id}\n"));
}

/// Makes `task_id` depend on `predecessor_id` via `bala dep add`.
fn add_dependency(db_path: &std::path::Path, task_id: &str, predecessor_id: &str) {
    bala_cmd(db_path)
        .args(["dep", "add", task_id, "--on", predecessor_id])
        .assert()
        .success();
}

#[test]
fn task_delete_should_name_dependent_tasks_before_confirming() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a_id = add_task(&db_path, &["--title", "Task A"]);
    let b_id = add_task(&db_path, &["--title", "Task B"]);
    add_dependency(&db_path, &b_id, &a_id);

    bala_cmd(&db_path)
        .args(["task", "delete", &a_id])
        .write_stdin("n\n")
        .assert()
        .success()
        .stderr(contains("1 task(s) depend on what this deletes:"))
        .stderr(contains(format!("  {b_id} Task B")))
        .stdout(contains("Aborted"));

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("Task A"));
}

#[test]
fn task_delete_with_yes_should_still_print_the_dependents_warning() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a_id = add_task(&db_path, &["--title", "Task A"]);
    let b_id = add_task(&db_path, &["--title", "Task B"]);
    add_dependency(&db_path, &b_id, &a_id);

    let output = bala_cmd(&db_path)
        .args(["task", "delete", &a_id, "--yes"])
        .assert()
        .success()
        .stderr(contains("1 task(s) depend on what this deletes:"))
        .stderr(contains(format!("  {b_id} Task B")))
        .get_output()
        .stdout
        .clone();

    assert_eq!(String::from_utf8(output).unwrap(), format!("{a_id}\n"));
}

#[test]
fn task_delete_cascade_should_warn_about_every_dependent_outside_the_subtree() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let root_id = add_task(&db_path, &["--title", "Subtree root"]);
    let child_id = add_task(
        &db_path,
        &["--title", "Subtree child", "--parent", &root_id],
    );
    let grandchild_id = add_task(
        &db_path,
        &["--title", "Subtree grandchild", "--parent", &child_id],
    );
    let inside_id = add_task(
        &db_path,
        &["--title", "Subtree dependent", "--parent", &root_id],
    );
    add_dependency(&db_path, &inside_id, &grandchild_id);
    let on_root_id = add_task(&db_path, &["--title", "Depends on root"]);
    add_dependency(&db_path, &on_root_id, &root_id);
    let on_grandchild_id = add_task(&db_path, &["--title", "Depends on grandchild"]);
    add_dependency(&db_path, &on_grandchild_id, &grandchild_id);

    bala_cmd(&db_path)
        .args(["task", "delete", &root_id, "--cascade", "--yes"])
        .assert()
        .success()
        .stderr(contains("2 task(s) depend on what this deletes:"))
        .stderr(contains(format!("  {on_root_id} Depends on root")).count(1))
        .stderr(contains(format!("  {on_grandchild_id} Depends on grandchild")).count(1))
        .stderr(contains(inside_id).not());

    let ls_text = task_ls(&db_path);
    assert!(!ls_text.contains("Subtree"), "no survivors: {ls_text}");
    assert!(ls_text.contains("Depends on root"), "{ls_text}");
    assert!(ls_text.contains("Depends on grandchild"), "{ls_text}");
}

#[test]
fn task_delete_promote_children_should_warn_only_about_the_task_itself() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_id = add_task(&db_path, &["--title", "Parent task"]);
    let child_id = add_task(&db_path, &["--title", "Child task", "--parent", &parent_id]);
    let on_child_id = add_task(&db_path, &["--title", "Depends on child"]);
    add_dependency(&db_path, &on_child_id, &child_id);
    let on_parent_id = add_task(&db_path, &["--title", "Depends on parent"]);
    add_dependency(&db_path, &on_parent_id, &parent_id);

    bala_cmd(&db_path)
        .args(["task", "delete", &parent_id, "--promote-children", "--yes"])
        .assert()
        .success()
        .stderr(contains("1 task(s) depend on what this deletes:"))
        .stderr(contains(format!("  {on_parent_id} Depends on parent")))
        .stderr(contains(on_child_id).not());
}

#[test]
fn task_delete_without_dependents_should_print_no_warning() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a_id = add_task(&db_path, &["--title", "Task A"]);
    let b_id = add_task(&db_path, &["--title", "Task B"]);
    add_dependency(&db_path, &a_id, &b_id);

    bala_cmd(&db_path)
        .args(["task", "delete", &a_id, "--yes"])
        .assert()
        .success()
        .stderr(contains("depend on what this deletes").not());
}

#[test]
fn task_restore_should_bring_task_back_into_ls() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Leaf task"]);

    bala_cmd(&db_path)
        .args(["task", "delete", &task_id, "--yes"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "restore", &task_id])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("Leaf task"));
}

#[test]
fn task_restore_with_unknown_id_should_fail() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let unknown_task_id = uuid::Uuid::new_v4().to_string();

    bala_cmd(&db_path)
        .args(["task", "restore", &unknown_task_id])
        .assert()
        .failure();
}

#[test]
fn task_complete_leaf_task_should_show_as_complete_in_ls() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Leaf task"]);

    bala_cmd(&db_path)
        .args(["task", "complete", &task_id])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("[x]").and(contains("Leaf task")));
}

#[test]
fn task_complete_with_unknown_task_id_should_fail() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let unknown_task_id = uuid::Uuid::new_v4().to_string();

    bala_cmd(&db_path)
        .args(["task", "complete", &unknown_task_id])
        .assert()
        .failure();
}

#[test]
fn task_complete_task_with_incomplete_children_should_fail_with_warning() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_id = add_task(&db_path, &["--title", "Parent task"]);
    add_task(&db_path, &["--title", "Child task", "--parent", &parent_id]);

    bala_cmd(&db_path)
        .args(["task", "complete", &parent_id])
        .assert()
        .failure()
        .stderr(contains("incomplete children").and(contains("cascade")));
}

#[test]
fn task_complete_task_with_incomplete_children_and_cascade_should_complete_all() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_id = add_task(&db_path, &["--title", "Parent task"]);
    add_task(&db_path, &["--title", "Child task", "--parent", &parent_id]);

    bala_cmd(&db_path)
        .args(["task", "complete", &parent_id, "--cascade"])
        .assert()
        .success();

    let ls_output = bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ls_text = String::from_utf8(ls_output).unwrap();

    let parent_line = ls_text
        .lines()
        .find(|line| line.contains("Parent task"))
        .expect("parent line present");
    let child_line = ls_text
        .lines()
        .find(|line| line.contains("Child task"))
        .expect("child line present");

    assert!(
        parent_line.contains("[x]"),
        "parent not marked complete: {parent_line:?}"
    );
    assert!(
        child_line.contains("[x]"),
        "child not marked complete: {child_line:?}"
    );
}

#[test]
fn task_reopen_completed_task_should_show_as_incomplete_in_ls() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Leaf task"]);

    bala_cmd(&db_path)
        .args(["task", "complete", &task_id])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "reopen", &task_id])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("[ ]").and(contains("Leaf task")));
}

#[test]
fn task_reopen_with_unknown_task_id_should_fail() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let unknown_task_id = uuid::Uuid::new_v4().to_string();

    bala_cmd(&db_path)
        .args(["task", "reopen", &unknown_task_id])
        .assert()
        .failure();
}

#[test]
fn task_reopen_already_incomplete_task_should_succeed() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Never completed"]);

    bala_cmd(&db_path)
        .args(["task", "reopen", &task_id])
        .assert()
        .success()
        .stdout(contains("[ ]").and(contains("Never completed")));
}

#[test]
fn bala_with_no_subcommand_on_non_tty_should_fail_gracefully() {
    // No subcommand => the TUI path. `assert_cmd`'s `Command` runs with
    // stdin/stdout not attached to a real TTY, so `enable_raw_mode()` should
    // fail fast (propagated as `CliError::TerminalIo`) rather than the
    // process hanging waiting for terminal input. `.timeout(..)` guards
    // against a regression turning this into a hang that blocks the whole
    // test suite instead of failing promptly.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    bala_cmd(&db_path)
        .timeout(Duration::from_secs(10))
        .assert()
        .failure()
        .stderr(contains("terminal I/O error"));
}

#[test]
fn task_ls_should_show_incomplete_marker_for_a_new_task() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    add_task(&db_path, &["--title", "Fresh task"]);

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("[ ]").and(contains("Fresh task")));
}

#[test]
fn task_mv_with_parent_should_reparent_task() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let movable_id = add_task(&db_path, &["--title", "Task A"]);
    let new_parent_id = add_task(&db_path, &["--title", "Task B"]);

    bala_cmd(&db_path)
        .args(["task", "mv", &movable_id, "--parent", &new_parent_id])
        .assert()
        .success();

    let ls_output = bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ls_text = String::from_utf8(ls_output).unwrap();

    let a_line = ls_text
        .lines()
        .find(|line| line.contains("Task A"))
        .expect("task A line present");
    let b_line = ls_text
        .lines()
        .find(|line| line.contains("Task B"))
        .expect("task B line present");

    let a_indent = a_line.len() - a_line.trim_start().len();
    let b_indent = b_line.len() - b_line.trim_start().len();
    assert!(
        a_indent > b_indent,
        "expected reparented task A to be indented more than top-level task B: a={a_line:?} b={b_line:?}"
    );
}

#[test]
fn task_mv_without_parent_should_promote_to_top_level() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_id = add_task(&db_path, &["--title", "Parent task"]);
    let child_id = add_task(&db_path, &["--title", "Child task", "--parent", &parent_id]);

    bala_cmd(&db_path)
        .args(["task", "mv", &child_id])
        .assert()
        .success();

    let ls_output = bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ls_text = String::from_utf8(ls_output).unwrap();

    let child_line = ls_text
        .lines()
        .find(|line| line.contains("Child task"))
        .expect("child line present");
    let child_indent = child_line.len() - child_line.trim_start().len();
    assert_eq!(
        child_indent, 0,
        "expected promoted child to be top-level (no indent): {child_line:?}"
    );
}

#[test]
fn task_add_with_inherit_should_copy_assignee_and_dates_from_first_parent() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let user_id = String::from_utf8(
        bala_cmd(&db_path)
            .args(["user", "add", "--name", "Ada"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_owned();

    let parent_id = add_task(
        &db_path,
        &[
            "--title",
            "Parent task",
            "--assignee",
            &user_id,
            "--start",
            "2026-01-01",
            "--due",
            "2026-01-31",
        ],
    );

    add_task(
        &db_path,
        &["--title", "Child task", "--parent", &parent_id, "--inherit"],
    );

    // The child's line carries the parent's assignee (by name, never by
    // id) and both of its dates.
    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(
            contains("Child task (type: task) (assigned: Ada) 2026-01-01..2026-01-31")
                .and(contains(&user_id).not()),
        );
}

#[test]
fn task_add_with_inherit_and_explicit_assignee_should_prefer_explicit_value() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_user_id = String::from_utf8(
        bala_cmd(&db_path)
            .args(["user", "add", "--name", "Ada"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_owned();

    let child_user_id = String::from_utf8(
        bala_cmd(&db_path)
            .args(["user", "add", "--name", "Grace"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_owned();

    let parent_id = add_task(
        &db_path,
        &["--title", "Parent task", "--assignee", &parent_user_id],
    );

    add_task(
        &db_path,
        &[
            "--title",
            "Child task",
            "--parent",
            &parent_id,
            "--inherit",
            "--assignee",
            &child_user_id,
        ],
    );

    let ls_output = bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ls_text = String::from_utf8(ls_output).unwrap();

    let child_line = ls_text
        .lines()
        .find(|line| line.contains("Child task"))
        .expect("child line present");
    assert!(
        child_line.contains("Grace") && !child_line.contains("Ada"),
        "expected child to show the explicit assignee, not the inherited one: {child_line:?}"
    );
}

#[test]
fn task_add_with_inherit_should_leave_fields_unset_when_parent_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_id = add_task(&db_path, &["--title", "Parent task"]);

    add_task(
        &db_path,
        &["--title", "Child task", "--parent", &parent_id, "--inherit"],
    );

    let ls_output = bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ls_text = String::from_utf8(ls_output).unwrap();

    let child_line = ls_text
        .lines()
        .find(|line| line.contains("Child task"))
        .expect("child line present");
    assert!(
        !child_line.contains("assigned:"),
        "expected child to have no assignee: {child_line:?}"
    );
}

#[test]
fn task_add_with_inherit_and_no_parent_should_fail_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    bala_cmd(&db_path)
        .args(["task", "add", "--title", "X", "--inherit"])
        .assert()
        .failure();
}

#[test]
fn task_mv_with_circular_parent_should_fail_with_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let task_id = add_task(&db_path, &["--title", "Task A"]);

    bala_cmd(&db_path)
        .args(["task", "mv", &task_id, "--parent", &task_id])
        .assert()
        .failure()
        .stderr(contains("would make it its own ancestor"));
}

#[test]
fn task_mv_with_parents_flag_should_fail_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_id = add_task(&db_path, &["--title", "Parent task"]);
    let child_id = add_task(&db_path, &["--title", "Child task", "--parent", &parent_id]);
    let other_id = add_task(&db_path, &["--title", "Other task"]);

    bala_cmd(&db_path)
        .args(["task", "mv", &child_id, "--parents", &other_id])
        .assert()
        .code(2)
        .stderr(contains("--parents"));

    // The rejected move left the child under its original parent.
    let ls_output = bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ls_text = String::from_utf8(ls_output).unwrap();
    let child_line = ls_text
        .lines()
        .find(|line| line.contains("Child task"))
        .expect("child line present");
    assert!(
        child_line.starts_with("  "),
        "expected child to stay nested: {child_line:?}"
    );
}

#[test]
fn task_add_with_unknown_type_should_fail_with_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    bala_cmd(&db_path)
        .args([
            "task",
            "add",
            "--title",
            "Ship it",
            "--type",
            "bogus-nonexistent-type",
        ])
        .assert()
        .failure();
}

#[test]
fn task_ls_with_type_filter_should_show_only_tasks_of_that_type() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    add_task(&db_path, &["--title", "Typed task"]);

    bala_cmd(&db_path)
        .args(["task", "ls", "--type", "task"])
        .assert()
        .success()
        .stdout(contains("Typed task"));
}

#[test]
fn task_ls_with_type_filter_for_unused_type_should_show_no_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    add_task(&db_path, &["--title", "Some task"]);

    bala_cmd(&db_path)
        .args(["task", "ls", "--type", "nonexistent"])
        .assert()
        .success()
        .stdout(contains("Some task").not());
}

#[test]
fn task_ls_should_print_one_line_per_task() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let goal_id = add_task(&db_path, &["--title", "Goal"]);
    let left_id = add_task(&db_path, &["--title", "Left", "--parent", &goal_id]);
    let right_id = add_task(&db_path, &["--title", "Right", "--parent", &goal_id]);
    let left_leaf_id = add_task(&db_path, &["--title", "Left leaf", "--parent", &left_id]);
    let right_leaf_id = add_task(&db_path, &["--title", "Right leaf", "--parent", &right_id]);
    let deep_id = add_task(&db_path, &["--title", "Deep", "--parent", &right_leaf_id]);
    let other_id = add_task(&db_path, &["--title", "Other root"]);
    let ids = [
        goal_id,
        left_id,
        right_id,
        left_leaf_id,
        right_leaf_id,
        deep_id,
        other_id,
    ];

    let ls_text = task_ls(&db_path);

    assert_eq!(ls_text.lines().count(), ids.len(), "{ls_text}");
    for id in &ids {
        assert_eq!(
            lines_naming(&ls_text, id).len(),
            1,
            "{id} should be on exactly one line: {ls_text}"
        );
    }
}

#[test]
fn task_ls_should_show_each_task_s_type() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    bala_cmd(&db_path)
        .args(["type", "set", "milestone", "--label", "Milestone"])
        .assert()
        .success();
    add_task(&db_path, &["--title", "Ship v1", "--type", "milestone"]);

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("milestone"));
}

#[test]
fn type_ls_should_include_the_seeded_default_task_type() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    // `Core::new` seeds a default `"task"` type on first open; force that
    // open by running any command before `type ls`.
    add_task(&db_path, &["--title", "Trigger seeding"]);

    bala_cmd(&db_path)
        .args(["type", "ls"])
        .assert()
        .success()
        .stdout(contains("task"));
}

#[test]
fn type_set_should_create_a_new_type_and_type_ls_should_include_it() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    bala_cmd(&db_path)
        .args(["type", "set", "goal", "--label", "Goal"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["type", "ls"])
        .assert()
        .success()
        .stdout(contains("goal"))
        .stdout(contains("Goal"));
}

#[test]
fn type_set_should_update_label_without_losing_previously_set_color() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    bala_cmd(&db_path)
        .args(["type", "set", "goal", "--label", "Goal", "--color", "blue"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["type", "set", "goal", "--label", "Goal Updated"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["type", "ls"])
        .assert()
        .success()
        .stdout(contains("Goal Updated"))
        .stdout(contains("blue"));
}

#[test]
fn task_add_with_type_set_by_type_set_should_succeed() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    bala_cmd(&db_path)
        .args(["type", "set", "goal", "--label", "Goal"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "add", "--title", "x", "--type", "goal"])
        .assert()
        .success();
}

#[test]
fn dep_add_should_print_the_task_with_its_predecessor() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a = add_task(&db_path, &["--title", "A"]);
    let b = add_task(&db_path, &["--title", "B"]);

    bala_cmd(&db_path)
        .args(["dep", "add", &b, "--on", &a])
        .assert()
        .success()
        .stdout(contains("B"))
        .stdout(contains(format!("  depends on: {a} (fs)")));
}

#[test]
fn dep_add_twice_should_list_both_predecessors() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a = add_task(&db_path, &["--title", "A"]);
    let b = add_task(&db_path, &["--title", "B"]);
    let c = add_task(&db_path, &["--title", "C"]);

    bala_cmd(&db_path)
        .args(["dep", "add", &c, "--on", &a])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["dep", "add", &c, "--on", &b])
        .assert()
        .success()
        .stdout(contains(format!("  depends on: {a} (fs)")))
        .stdout(contains(format!("  depends on: {b} (fs)")));
}

#[test]
fn dep_add_should_default_to_finish_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a = add_task(&db_path, &["--title", "A"]);
    let b = add_task(&db_path, &["--title", "B"]);

    bala_cmd(&db_path)
        .args(["dep", "add", &b, "--on", &a])
        .assert()
        .success()
        .stdout(contains("(fs)"))
        .stdout(contains("(ss)").not())
        .stdout(contains("(ff)").not())
        .stdout(contains("(sf)").not());
}

#[test]
fn dep_add_with_type_should_record_that_type() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a = add_task(&db_path, &["--title", "A"]);
    let b = add_task(&db_path, &["--title", "B"]);

    bala_cmd(&db_path)
        .args(["dep", "add", &b, "--on", &a, "--type", "ss"])
        .assert()
        .success()
        .stdout(contains(format!("  depends on: {a} (ss)")));
}

#[test]
fn dep_add_with_unknown_type_should_fail_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a = add_task(&db_path, &["--title", "A"]);
    let b = add_task(&db_path, &["--title", "B"]);

    bala_cmd(&db_path)
        .args(["dep", "add", &b, "--on", &a, "--type", "xx"])
        .assert()
        .failure()
        .code(2)
        .stderr(contains("invalid value 'xx'"));
}

#[test]
fn dep_add_on_itself_should_fail_with_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a = add_task(&db_path, &["--title", "A"]);

    bala_cmd(&db_path)
        .args(["dep", "add", &a, "--on", &a])
        .assert()
        .failure()
        .stderr(contains("cannot depend on itself"));
}

#[test]
fn dep_add_on_own_parent_should_fail_with_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let p = add_task(&db_path, &["--title", "P"]);
    let c = add_task(&db_path, &["--title", "C", "--parent", &p]);

    bala_cmd(&db_path)
        .args(["dep", "add", &c, "--on", &p])
        .assert()
        .failure()
        .stderr(contains("ancestor"));
}

#[test]
fn dep_add_closing_a_cycle_should_fail_with_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a = add_task(&db_path, &["--title", "A"]);
    let b = add_task(&db_path, &["--title", "B"]);

    bala_cmd(&db_path)
        .args(["dep", "add", &b, "--on", &a])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["dep", "add", &a, "--on", &b])
        .assert()
        .failure()
        .stderr(contains("cycle"));
}

#[test]
fn dep_rm_should_remove_the_predecessor() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a = add_task(&db_path, &["--title", "A"]);
    let b = add_task(&db_path, &["--title", "B"]);

    bala_cmd(&db_path)
        .args(["dep", "add", &b, "--on", &a])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["dep", "rm", &b, "--on", &a])
        .assert()
        .success()
        .stdout(contains("B"))
        .stdout(contains("depends on:").not());
}

#[test]
fn dep_rm_with_unknown_task_id_should_fail() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let a = add_task(&db_path, &["--title", "A"]);

    bala_cmd(&db_path)
        .args([
            "dep",
            "rm",
            "00000000-0000-0000-0000-000000000000",
            "--on",
            &a,
        ])
        .assert()
        .failure()
        .stderr(contains("not found"));
}

#[test]
fn task_ls_should_mark_out_of_sync_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let a = add_task(
        &db_path,
        &[
            "--title",
            "A",
            "--start",
            "2026-10-01",
            "--due",
            "2026-10-05",
        ],
    );
    let b = add_task(
        &db_path,
        &[
            "--title",
            "B",
            "--start",
            "2026-10-05",
            "--due",
            "2026-10-08",
        ],
    );
    add_dependency(&db_path, &b, &a);
    let in_sync = format!("[ ] {b} B (type: task) 2026-10-05..2026-10-08 (fixed)");
    assert_eq!(ls_line(&db_path, &b), in_sync);

    // Moving A's due date past B's start breaks B's dependency on it.
    bala_cmd(&db_path)
        .args(["task", "edit", &a, "--due", "2026-10-06"])
        .assert()
        .success()
        .stdout(contains("(out of sync)").not());

    // B keeps its dates and gains the marker; A, which breaks nothing of
    // its own, does not.
    assert_eq!(ls_line(&db_path, &b), format!("{in_sync} (out of sync)"));
    assert_eq!(
        ls_line(&db_path, &a),
        format!("[ ] {a} A (type: task) 2026-10-01..2026-10-06 (fixed)")
    );
}

#[test]
fn dep_add_on_a_violating_pair_should_print_the_task_as_out_of_sync() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let a = add_task(
        &db_path,
        &[
            "--title",
            "A",
            "--start",
            "2026-10-01",
            "--due",
            "2026-10-05",
        ],
    );
    let b = add_task(
        &db_path,
        &[
            "--title",
            "B",
            "--start",
            "2026-10-03",
            "--due",
            "2026-10-08",
        ],
    );

    let output = bala_cmd(&db_path)
        .args(["dep", "add", &b, "--on", &a])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // The dependency is recorded and neither task has moved.
    assert_eq!(
        String::from_utf8(output).unwrap(),
        format!(
            "[ ] {b} B (type: task) 2026-10-03..2026-10-08 (fixed) (out of sync)\n  \
             depends on: {a} (fs)\n"
        )
    );
    assert_eq!(
        ls_line(&db_path, &a),
        format!("[ ] {a} A (type: task) 2026-10-01..2026-10-05 (fixed)")
    );
}

/// Adds a task with both dates set, returning its id.
fn add_dated_task(db_path: &std::path::Path, title: &str, start: &str, due: &str) -> String {
    add_task(db_path, &["--title", title, "--start", start, "--due", due])
}

/// Builds A (`2026-10-01..2026-10-05`) and B (`2026-10-05..2026-10-08`),
/// B depending on A, then pushes A's due date to `2026-10-09` so B breaks
/// its dependency. B floats when `float_b` is set and stays fixed otherwise.
/// Returns `(a, b)`.
fn late_predecessor_fixture(db_path: &std::path::Path, float_b: bool) -> (String, String) {
    let a = add_dated_task(db_path, "A", "2026-10-01", "2026-10-05");
    let b = add_dated_task(db_path, "B", "2026-10-05", "2026-10-08");
    if float_b {
        bala_cmd(db_path)
            .args(["task", "edit", &b, "--float"])
            .assert()
            .success();
    }
    add_dependency(db_path, &b, &a);
    bala_cmd(db_path)
        .args(["task", "edit", &a, "--due", "2026-10-09"])
        .assert()
        .success();
    (a, b)
}

/// Runs `bala schedule` with `args`, feeding `stdin`, and returns its
/// stdout.
fn schedule_stdout(db_path: &std::path::Path, args: &[&str], stdin: &str) -> String {
    let output = bala_cmd(db_path)
        .arg("schedule")
        .args(args)
        .write_stdin(stdin)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output).unwrap()
}

#[test]
fn schedule_should_list_moves_with_old_and_new_dates_before_confirming() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let (_a, b) = late_predecessor_fixture(&db_path, true);

    let stdout = schedule_stdout(&db_path, &[], "n\n");

    let preview = format!(
        "1 task(s) would move:\n  \
         {b} B: 2026-10-05..2026-10-08 -> 2026-10-09..2026-10-12\n\
         Apply? [y/N] "
    );
    assert!(
        stdout.starts_with(&preview),
        "expected the preview and then the prompt, got: {stdout}"
    );
}

#[test]
fn schedule_answered_no_should_change_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let (_a, b) = late_predecessor_fixture(&db_path, true);
    let before = task_ls(&db_path);

    let stdout = schedule_stdout(&db_path, &[], "n\n");

    assert!(
        stdout.ends_with("Aborted: nothing was moved.\n"),
        "expected the abort message, got: {stdout}"
    );
    assert_eq!(task_ls(&db_path), before);
    assert_eq!(
        ls_line(&db_path, &b),
        format!("[ ] {b} B (type: task) 2026-10-05..2026-10-08 (out of sync)")
    );
}

#[test]
fn schedule_answered_yes_should_move_the_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let (a, b) = late_predecessor_fixture(&db_path, true);

    let stdout = schedule_stdout(&db_path, &[], "y\n");

    // B moves, stays floating, and is back in sync; A is untouched.
    let moved = format!("[ ] {b} B (type: task) 2026-10-09..2026-10-12");
    assert!(
        stdout.ends_with(&format!("Apply? [y/N] {moved}\n")),
        "expected the moved task after the prompt, got: {stdout}"
    );
    assert_eq!(ls_line(&db_path, &b), moved);
    assert_eq!(
        ls_line(&db_path, &a),
        format!("[ ] {a} A (type: task) 2026-10-01..2026-10-09 (fixed)")
    );
}

#[test]
fn schedule_with_yes_should_apply_without_prompting() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let (_a, b) = late_predecessor_fixture(&db_path, true);

    // No stdin is fed: a prompt would read end-of-input and decline.
    let stdout = schedule_stdout(&db_path, &["--yes"], "");

    let moved = format!("[ ] {b} B (type: task) 2026-10-09..2026-10-12");
    assert_eq!(
        stdout,
        format!(
            "1 task(s) would move:\n  \
             {b} B: 2026-10-05..2026-10-08 -> 2026-10-09..2026-10-12\n\
             {moved}\n"
        )
    );
    assert_eq!(ls_line(&db_path, &b), moved);
}

#[test]
fn schedule_should_list_tasks_left_out_of_sync() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let (a, b) = late_predecessor_fixture(&db_path, true);
    // C is fixed and starts before A finishes, so no reschedule can fix it.
    let c = add_dated_task(&db_path, "C", "2026-10-06", "2026-10-07");
    add_dependency(&db_path, &c, &a);

    let stdout = schedule_stdout(&db_path, &[], "n\n");

    let preview = format!(
        "1 task(s) would move:\n  \
         {b} B: 2026-10-05..2026-10-08 -> 2026-10-09..2026-10-12\n\
         1 task(s) would stay out of sync:\n  \
         {c} C\n\
         Apply? [y/N] "
    );
    assert!(
        stdout.starts_with(&preview),
        "expected the out-of-sync list before the prompt, got: {stdout}"
    );
}

#[test]
fn schedule_should_not_move_a_fixed_task() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let (_a, b) = late_predecessor_fixture(&db_path, false);
    let before = task_ls(&db_path);

    let stdout = schedule_stdout(&db_path, &["--yes"], "");

    // The fixed task is reported, not moved.
    assert_eq!(
        stdout,
        format!("Nothing to move.\n1 task(s) would stay out of sync:\n  {b} B\n")
    );
    assert_eq!(task_ls(&db_path), before);
    assert_eq!(
        ls_line(&db_path, &b),
        format!("[ ] {b} B (type: task) 2026-10-05..2026-10-08 (fixed) (out of sync)")
    );
}

#[test]
fn schedule_with_nothing_to_move_should_say_so_and_not_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let a = add_dated_task(&db_path, "A", "2026-10-01", "2026-10-05");
    let b = add_dated_task(&db_path, "B", "2026-10-05", "2026-10-08");
    add_dependency(&db_path, &b, &a);

    // Answering "y" to a prompt that is never shown changes nothing.
    let stdout = schedule_stdout(&db_path, &[], "y\n");

    assert_eq!(stdout, "Nothing to move.\n");
}

#[test]
fn schedule_should_fill_a_due_date_from_duration() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");
    let a = add_task(
        &db_path,
        &["--title", "A", "--start", "2026-10-05", "--duration", "3"],
    );
    bala_cmd(&db_path)
        .args(["task", "edit", &a, "--float"])
        .assert()
        .success();
    // Fixed, so its missing due date is left alone.
    let fixed = add_task(
        &db_path,
        &[
            "--title",
            "Fixed",
            "--start",
            "2026-10-05",
            "--duration",
            "3",
        ],
    );

    let stdout = schedule_stdout(&db_path, &["--yes"], "");

    assert_eq!(
        stdout,
        format!(
            "1 task(s) would move:\n  \
             {a} A: 2026-10-05..? -> 2026-10-05..2026-10-08\n\
             [ ] {a} A (type: task) 2026-10-05..2026-10-08 (3d)\n"
        )
    );
    assert_eq!(
        ls_line(&db_path, &fixed),
        format!("[ ] {fixed} Fixed (type: task) 2026-10-05..? (3d) (fixed)")
    );
}
