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

#[test]
fn task_delete_task_with_subtasks_and_promote_children_should_keep_child_visible() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let parent_id = add_task(&db_path, &["--title", "Parent task"]);
    add_task(&db_path, &["--title", "Child task", "--parent", &parent_id]);

    bala_cmd(&db_path)
        .args(["task", "delete", &parent_id, "--promote-children", "--yes"])
        .assert()
        .success();

    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(contains("Parent task").not().and(contains("Child task")));
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
fn task_mv_should_reparent_task_and_show_new_parent_in_ls() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bala.db");

    let movable_id = add_task(&db_path, &["--title", "Task A"]);
    let new_parent_id = add_task(&db_path, &["--title", "Task B"]);

    bala_cmd(&db_path)
        .args(["task", "mv", &movable_id, "--parents", &new_parent_id])
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
fn task_mv_with_empty_parents_should_promote_to_top_level() {
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

    // `task ls` doesn't print start/due dates at all (see `format_task_line`
    // in `bala-cli/src/cli.rs`), so the date-inheritance half of this
    // behavior isn't observable through CLI stdout. The assignee-name
    // suffix is, so that's what's asserted here as the observable proxy for
    // "inherit actually ran".
    bala_cmd(&db_path)
        .args(["task", "ls"])
        .assert()
        .success()
        .stdout(
            contains("Child task")
                .and(contains("Ada"))
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
        .args(["task", "mv", &task_id, "--parents", &task_id])
        .assert()
        .failure();
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
