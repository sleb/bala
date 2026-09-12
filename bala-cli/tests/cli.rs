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
    // visible-output assertion available here — the observable surface for
    // this checkpoint is just that the edit itself succeeds.
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
