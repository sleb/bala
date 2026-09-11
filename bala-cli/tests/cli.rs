//! Integration tests for the `bala` binary, driven end-to-end via
//! `assert_cmd`. Each test points the CLI at its own temp-file SQLite
//! database via the `--db-path` flag, so tests never touch the real
//! default OS data dir and never interfere with each other.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn bala_cmd(db_path: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("bala-cli").unwrap();
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
