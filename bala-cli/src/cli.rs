//! Command dispatch: builds a `Core<SqliteStore>` over the resolved db path
//! and drives it from parsed CLI arguments.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use bala_core::{
    Core, CoreError, DeleteMode, Field, NewTask, StoreError, Task, TaskId, TaskPatch, TaskStatus,
    TaskType, TreeFilter, UserId,
};
use bala_store::SqliteStore;
use chrono::NaiveDate;
use clap::{Args, Parser, Subcommand};
use uuid::Uuid;

/// Top-level CLI arguments.
#[derive(Debug, Parser)]
#[command(name = "bala", about = "A task tracker")]
pub struct Cli {
    /// Overrides the default SQLite database path. Mainly for tests; the
    /// default lives in the OS-standard data directory (see `config`).
    #[arg(long, global = true)]
    pub db_path: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Task operations: `add`, `ls`, `edit`, `delete`, `restore`, `complete`,
    /// `reopen`.
    Task(TaskArgs),
    /// User operations: `add`, `ls`.
    User(UserArgs),
    /// Task type operations: `ls`, `set`.
    Type(TypeArgs),
}

#[derive(Debug, Args)]
pub struct TaskArgs {
    #[command(subcommand)]
    pub command: TaskCommands,
}

#[derive(Debug, Subcommand)]
pub enum TaskCommands {
    /// Create a new task.
    Add(AddArgs),
    /// List all tasks.
    Ls(LsArgs),
    /// Edit an existing task.
    Edit(EditArgs),
    /// Delete a task, optionally along with (or promoting) its subtasks.
    Delete(DeleteArgs),
    /// Restore a previously deleted task.
    Restore(RestoreArgs),
    /// Mark a task complete, optionally cascading to its subtasks.
    Complete(CompleteArgs),
    /// Reopen a previously completed task.
    Reopen(ReopenArgs),
    /// Reparent a task under a new set of parents (or none, to promote it
    /// to top-level).
    Mv(MvArgs),
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[arg(long)]
    pub title: String,

    #[arg(long)]
    pub description: Option<String>,

    /// May be given multiple times to attach the new task under several
    /// parents.
    #[arg(long = "parent")]
    pub parent: Vec<Uuid>,

    #[arg(long)]
    pub start: Option<NaiveDate>,

    #[arg(long)]
    pub due: Option<NaiveDate>,

    /// Id of the user to assign the new task to.
    #[arg(long)]
    pub assignee: Option<Uuid>,

    /// Copy assignee/dates from the first `--parent` (only meaningful with
    /// at least one `--parent`; clap enforces that via `requires =
    /// "parent"`).
    #[arg(long, requires = "parent")]
    pub inherit: bool,

    /// Task type key. Defaults to the seeded `"task"` type when omitted.
    #[arg(long = "type")]
    pub type_key: Option<String>,
}

#[derive(Debug, Args)]
pub struct LsArgs {
    /// Only show tasks with this type key.
    #[arg(long = "type")]
    pub type_key: Option<String>,
}

// The four `clear_*` flags below are independent boolean switches (one per
// clearable field), not overlapping state — a state machine or enum
// wouldn't fit clap's derive-based flag model any better.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Args)]
pub struct EditArgs {
    /// Id of the task to edit.
    pub id: Uuid,

    #[arg(long)]
    pub title: Option<String>,

    #[arg(long, conflicts_with = "clear_description")]
    pub description: Option<String>,

    /// Clear the task's description.
    #[arg(long)]
    pub clear_description: bool,

    #[arg(long, conflicts_with = "clear_start")]
    pub start: Option<NaiveDate>,

    /// Clear the task's start date.
    #[arg(long)]
    pub clear_start: bool,

    #[arg(long, conflicts_with = "clear_due")]
    pub due: Option<NaiveDate>,

    /// Clear the task's due date.
    #[arg(long)]
    pub clear_due: bool,

    /// Id of the user to assign the task to.
    #[arg(long, conflicts_with = "clear_assignee")]
    pub assignee: Option<Uuid>,

    /// Unassign the task.
    #[arg(long)]
    pub clear_assignee: bool,

    /// New task type key.
    #[arg(long = "type")]
    pub type_key: Option<String>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// Id of the task to delete.
    pub id: Uuid,

    /// Skip the interactive confirmation prompt.
    #[arg(long)]
    pub yes: bool,

    /// Delete the task's whole subtree along with it.
    #[arg(long, conflicts_with = "promote_children")]
    pub cascade: bool,

    /// Reattach the task's children to its own parents instead of deleting
    /// them.
    #[arg(long)]
    pub promote_children: bool,
}

#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// Id of the task to restore.
    pub id: Uuid,
}

#[derive(Debug, Args)]
pub struct CompleteArgs {
    /// Id of the task to complete.
    pub id: Uuid,

    /// Also complete every incomplete descendant.
    #[arg(long)]
    pub cascade: bool,
}

#[derive(Debug, Args)]
pub struct ReopenArgs {
    /// Id of the task to reopen.
    pub id: Uuid,
}

#[derive(Debug, Args)]
pub struct MvArgs {
    /// Id of the task to reparent.
    pub id: Uuid,

    /// New parent ids, comma-separated. Omit (empty) to promote the task
    /// to top-level.
    #[arg(long, value_delimiter = ',')]
    pub parents: Vec<Uuid>,
}

#[derive(Debug, Args)]
pub struct UserArgs {
    #[command(subcommand)]
    pub command: UserCommands,
}

#[derive(Debug, Subcommand)]
pub enum UserCommands {
    /// Create a new user.
    Add(UserAddArgs),
    /// List all users.
    Ls,
}

#[derive(Debug, Args)]
pub struct UserAddArgs {
    #[arg(long)]
    pub name: String,
}

#[derive(Debug, Args)]
pub struct TypeArgs {
    #[command(subcommand)]
    pub command: TypeCommands,
}

#[derive(Debug, Subcommand)]
pub enum TypeCommands {
    /// List all configured task types.
    Ls,
    /// Create or update a task type.
    Set(TypeSetArgs),
}

#[derive(Debug, Args)]
pub struct TypeSetArgs {
    /// The type's key.
    pub key: String,

    #[arg(long)]
    pub label: String,

    #[arg(long)]
    pub color: Option<String>,

    #[arg(long, value_name = "N")]
    pub sort_order: Option<i32>,
}

/// Errors that can surface while dispatching a command, distinct from
/// `CoreError` only in that it also covers opening the store itself.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("failed to open database at {path}: {source}")]
    OpenStore {
        path: PathBuf,
        #[source]
        source: StoreError,
    },

    #[error(transparent)]
    Core(#[from] CoreError),

    /// The target task has subtasks and neither `--cascade` nor
    /// `--promote-children` was given, so the caller must choose a mode
    /// before anything is deleted (AC2's "warned and can choose").
    #[error(
        "task {title:?} has subtasks that must be handled; pass --cascade or --promote-children:\n{}",
        children
            .iter()
            .map(|task| format!("  {} {}", Uuid::from(task.id), task.title))
            .collect::<Vec<_>>()
            .join("\n")
    )]
    NeedsDeleteMode { title: String, children: Vec<Task> },

    #[error("failed to read confirmation from stdin: {0}")]
    Io(#[from] std::io::Error),

    /// I/O failure from the TUI's terminal setup, draw, or input polling.
    ///
    /// Can't derive `#[from] std::io::Error` here since `Io` above already
    /// claims that `From` impl; thiserror can't derive two `From<io::Error>`
    /// impls on one enum, and "failed to read confirmation from stdin"
    /// wouldn't fit a terminal-setup failure anyway, so this gets its own
    /// message and callers use `.map_err(CliError::TerminalIo)` explicitly.
    #[error("terminal I/O error: {0}")]
    TerminalIo(std::io::Error),
}

pub(crate) fn open_core(db_path: &Path) -> Result<Core<SqliteStore>, CliError> {
    if let Some(parent) = db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        // `SqliteStore::open` doesn't create missing parent directories
        // itself; the CLI's the one with an opinion on where the db lives,
        // so it's the CLI's job to make sure that directory exists.
        let _ = std::fs::create_dir_all(parent);
    }
    let store = SqliteStore::open(db_path).map_err(|source| CliError::OpenStore {
        path: db_path.to_owned(),
        source,
    })?;
    Core::new(store).map_err(CliError::from)
}

/// Runs the given command against the store at `db_path`, printing to
/// stdout on success.
///
/// # Errors
///
/// Returns `Err` if the store can't be opened or the underlying `Core`
/// call fails.
pub fn run_task_command(db_path: &Path, command: TaskCommands) -> Result<(), CliError> {
    match command {
        TaskCommands::Add(args) => run_task_add(db_path, args),
        TaskCommands::Ls(args) => run_task_ls(db_path, &args),
        TaskCommands::Edit(args) => run_task_edit(db_path, args),
        TaskCommands::Delete(args) => run_task_delete(db_path, &args),
        TaskCommands::Restore(args) => run_task_restore(db_path, &args),
        TaskCommands::Complete(args) => run_task_complete(db_path, &args),
        TaskCommands::Reopen(args) => run_task_reopen(db_path, &args),
        TaskCommands::Mv(args) => run_task_mv(db_path, &args),
    }
}

/// Builds a `Field<T>` from a parsed CLI value and its paired `--clear-*`
/// flag: `Set` when a value was given, `Clear` when the clear flag was
/// passed, `Keep` otherwise. For fields with no `--clear-*` counterpart
/// (`title`, `type_key`), pass `clear: false`.
fn field_from<T>(value: Option<T>, clear: bool) -> Field<T> {
    match value {
        Some(v) => Field::Set(v),
        None if clear => Field::Clear,
        None => Field::Keep,
    }
}

fn run_task_add(db_path: &Path, args: AddArgs) -> Result<(), CliError> {
    let mut core = open_core(db_path)?;

    let mut assignee_id = args.assignee.map(UserId::from);
    let mut start_date = args.start;
    let mut due_date = args.due;

    if args.inherit
        && let Some(&first_parent) = args.parent.first()
        && let Some(parent) = core.get_task(TaskId::from(first_parent))?
    {
        assignee_id = assignee_id.or(parent.assignee_id);
        start_date = start_date.or(parent.start_date);
        due_date = due_date.or(parent.due_date);
    }

    let new_task = NewTask {
        title: args.title,
        description: args.description,
        parent_ids: args.parent.into_iter().map(TaskId::from).collect(),
        type_key: args.type_key,
        start_date,
        due_date,
        assignee_id,
    };
    let task = core.create_task(new_task)?;
    println!("{}", Uuid::from(task.id));
    Ok(())
}

fn run_task_ls(db_path: &Path, args: &LsArgs) -> Result<(), CliError> {
    let core = open_core(db_path)?;
    let filter = TreeFilter {
        type_key: args.type_key.clone(),
        ..TreeFilter::default()
    };
    let tasks = core.get_tree(filter)?;
    let names: HashMap<UserId, String> = core
        .list_users()?
        .into_iter()
        .map(|user| (user.id, user.name))
        .collect();
    for task in &tasks {
        println!("{}", format_task_line(task, &names));
    }
    Ok(())
}

/// Renders the one-line summary shared by `task ls`/`edit`/`delete`/
/// `restore`/`complete`/`reopen`:
/// `"{indent}[{marker}] {id} {title}{assignee_suffix}"`, where `marker` is
/// `x` for a completed task and a space otherwise, and `indent` nests one
/// level under a task's first parent (if any) so a child visibly nests
/// under it — full recursive tree layout is the TUI's job later. Indent is
/// derived from the task itself rather than taken as a parameter so every
/// call site computes it the same way.
fn format_task_line(task: &Task, names: &HashMap<UserId, String>) -> String {
    let indent = if task.parent_ids.is_empty() { "" } else { "  " };
    let assignee = task
        .assignee_id
        .and_then(|id| names.get(&id))
        .map(|name| format!(" (assigned: {name})"))
        .unwrap_or_default();
    let marker = if task.status == TaskStatus::Complete {
        'x'
    } else {
        ' '
    };
    format!(
        "{indent}[{marker}] {} {}{assignee}",
        Uuid::from(task.id),
        task.title
    )
}

fn run_task_edit(db_path: &Path, args: EditArgs) -> Result<(), CliError> {
    let mut core = open_core(db_path)?;
    let patch = TaskPatch {
        title: field_from(args.title, false),
        description: field_from(args.description, args.clear_description),
        start_date: field_from(args.start, args.clear_start),
        due_date: field_from(args.due, args.clear_due),
        assignee_id: field_from(args.assignee.map(UserId::from), args.clear_assignee),
        type_key: field_from(args.type_key, false),
    };
    let updated = core.update_task(TaskId::from(args.id), patch)?;
    let names: HashMap<UserId, String> = core
        .list_users()?
        .into_iter()
        .map(|user| (user.id, user.name))
        .collect();
    for task in &updated {
        println!("{}", format_task_line(task, &names));
    }
    Ok(())
}

fn run_task_delete(db_path: &Path, args: &DeleteArgs) -> Result<(), CliError> {
    let mut core = open_core(db_path)?;
    let target_id = TaskId::from(args.id);

    let target = core
        .get_task(target_id)?
        .ok_or(CoreError::NotFound(target_id))?;
    let children = core.list_children(target_id)?;

    let mode = match (args.cascade, args.promote_children) {
        (true, _) => DeleteMode::Subtree,
        (false, true) => DeleteMode::PromoteChildren,
        (false, false) if children.is_empty() => DeleteMode::Subtree,
        (false, false) => {
            return Err(CliError::NeedsDeleteMode {
                title: target.title,
                children,
            });
        }
    };

    if !args.yes && !confirm(&format!("Delete task {:?}? [y/N] ", target.title))? {
        println!("Aborted: nothing was deleted.");
        return Ok(());
    }

    let deleted = core.delete_task(target_id, mode)?;
    for task in &deleted {
        println!("{}", Uuid::from(task.id));
    }
    Ok(())
}

/// Prompts on stdout/stdin for a yes/no confirmation, returning `true` only
/// for (trimmed, case-insensitive) `y` or `yes`.
fn confirm(prompt: &str) -> Result<bool, CliError> {
    print!("{prompt}");
    std::io::stdout().flush()?;

    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

fn run_task_restore(db_path: &Path, args: &RestoreArgs) -> Result<(), CliError> {
    let mut core = open_core(db_path)?;
    let task = core.restore_task(TaskId::from(args.id))?;
    let names: HashMap<UserId, String> = core
        .list_users()?
        .into_iter()
        .map(|user| (user.id, user.name))
        .collect();
    println!("{}", format_task_line(&task, &names));
    Ok(())
}

fn run_task_complete(db_path: &Path, args: &CompleteArgs) -> Result<(), CliError> {
    let mut core = open_core(db_path)?;
    let completed = core.complete_task(TaskId::from(args.id), args.cascade)?;
    let names: HashMap<UserId, String> = core
        .list_users()?
        .into_iter()
        .map(|user| (user.id, user.name))
        .collect();
    for task in &completed {
        println!("{}", format_task_line(task, &names));
    }
    Ok(())
}

fn run_task_reopen(db_path: &Path, args: &ReopenArgs) -> Result<(), CliError> {
    let mut core = open_core(db_path)?;
    let task = core.reopen_task(TaskId::from(args.id))?;
    let names: HashMap<UserId, String> = core
        .list_users()?
        .into_iter()
        .map(|user| (user.id, user.name))
        .collect();
    println!("{}", format_task_line(&task, &names));
    Ok(())
}

fn run_task_mv(db_path: &Path, args: &MvArgs) -> Result<(), CliError> {
    let mut core = open_core(db_path)?;
    let new_parents = args.parents.iter().copied().map(TaskId::from).collect();
    let task = core.set_parents(TaskId::from(args.id), new_parents)?;
    let names: HashMap<UserId, String> = core
        .list_users()?
        .into_iter()
        .map(|user| (user.id, user.name))
        .collect();
    println!("{}", format_task_line(&task, &names));
    Ok(())
}

/// Runs the given user command against the store at `db_path`, printing to
/// stdout on success.
///
/// # Errors
///
/// Returns `Err` if the store can't be opened or the underlying `Core`
/// call fails.
pub fn run_user_command(db_path: &Path, command: UserCommands) -> Result<(), CliError> {
    match command {
        UserCommands::Add(args) => run_user_add(db_path, args),
        UserCommands::Ls => run_user_ls(db_path),
    }
}

fn run_user_add(db_path: &Path, args: UserAddArgs) -> Result<(), CliError> {
    let mut core = open_core(db_path)?;
    let user = core.create_user(args.name)?;
    println!("{}", Uuid::from(user.id));
    Ok(())
}

fn run_user_ls(db_path: &Path) -> Result<(), CliError> {
    let core = open_core(db_path)?;
    let users = core.list_users()?;
    for user in &users {
        println!("{} {}", Uuid::from(user.id), user.name);
    }
    Ok(())
}

/// Runs the given task-type command against the store at `db_path`,
/// printing to stdout on success.
///
/// # Errors
///
/// Returns `Err` if the store can't be opened or the underlying `Core`
/// call fails.
pub fn run_type_command(db_path: &Path, command: TypeCommands) -> Result<(), CliError> {
    match command {
        TypeCommands::Ls => run_type_ls(db_path),
        TypeCommands::Set(args) => run_type_set(db_path, args),
    }
}

fn run_type_ls(db_path: &Path) -> Result<(), CliError> {
    let core = open_core(db_path)?;
    let types = core.list_task_types()?;
    for t in &types {
        println!("{}", format_type_line(t));
    }
    Ok(())
}

/// Renders the one-line summary `type ls`/`type set` print:
/// `"<key> <label> [color=<color>] sort_order=<n>"`. `color` is omitted
/// entirely when unset, rather than printed as some placeholder, since an
/// absent color isn't a value worth confusing with a real one.
fn format_type_line(t: &TaskType) -> String {
    let color = t
        .color
        .as_deref()
        .map(|c| format!(" color={c}"))
        .unwrap_or_default();
    format!("{} {}{color} sort_order={}", t.key, t.label, t.sort_order)
}

/// Merges `args` onto any existing type with the same key (label always
/// overwrites; an omitted `--color`/`--sort-order` keeps the existing
/// type's current value, or defaults to `None`/`0` for a brand-new key)
/// before upserting.
fn run_type_set(db_path: &Path, args: TypeSetArgs) -> Result<(), CliError> {
    let mut core = open_core(db_path)?;
    let existing = core
        .list_task_types()?
        .into_iter()
        .find(|t| t.key == args.key);

    let merged = match existing {
        Some(existing) => TaskType {
            key: args.key,
            label: args.label,
            color: args.color.or(existing.color),
            sort_order: args.sort_order.unwrap_or(existing.sort_order),
        },
        None => TaskType {
            key: args.key,
            label: args.label,
            color: args.color,
            sort_order: args.sort_order.unwrap_or(0),
        },
    };

    let upserted = core.upsert_task_type(merged)?;
    println!("{}", format_type_line(&upserted));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for `task add --inherit`'s date-copying half.
    ///
    /// `format_task_line` (the only thing `bala task ls`/`task mv`/etc.
    /// print to stdout) never renders `start_date`/`due_date`, so the
    /// black-box integration tests in `tests/cli.rs` can only observe the
    /// assignee half of inheritance via stdout. This test calls
    /// `run_task_add` in-process instead and reads the result back through
    /// `Core::list_children` (bypassing stdout entirely) to directly assert
    /// the date fields were actually copied — not just the assignee.
    #[test]
    fn run_task_add_with_inherit_should_copy_dates_from_first_parent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("bala.db");

        let mut core = open_core(&db_path).unwrap();
        let start = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let due = NaiveDate::from_ymd_opt(2026, 1, 20).unwrap();
        let parent = core
            .create_task(NewTask {
                title: "Parent".to_string(),
                description: None,
                parent_ids: Vec::new(),
                type_key: None,
                start_date: Some(start),
                due_date: Some(due),
                assignee_id: None,
            })
            .unwrap();
        drop(core);

        run_task_add(
            &db_path,
            AddArgs {
                title: "Child".to_string(),
                description: None,
                parent: vec![Uuid::from(parent.id)],
                start: None,
                due: None,
                assignee: None,
                inherit: true,
                type_key: None,
            },
        )
        .unwrap();

        let core = open_core(&db_path).unwrap();
        let children = core.list_children(parent.id).unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].start_date, Some(start));
        assert_eq!(children[0].due_date, Some(due));
    }
}
