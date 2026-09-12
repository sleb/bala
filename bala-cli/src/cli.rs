//! Command dispatch: builds a `Core<SqliteStore>` over the resolved db path
//! and drives it from parsed CLI arguments.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use bala_core::{
    Core, CoreError, DeleteMode, Field, NewTask, StoreError, Task, TaskId, TaskPatch, TreeFilter,
    UserId,
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
    /// Task operations: `add`, `ls`, `edit`, `delete`, `restore`.
    Task(TaskArgs),
    /// User operations: `add`, `ls`.
    User(UserArgs),
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
    Ls,
    /// Edit an existing task.
    Edit(EditArgs),
    /// Delete a task, optionally along with (or promoting) its subtasks.
    Delete(DeleteArgs),
    /// Restore a previously deleted task.
    Restore(RestoreArgs),
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
}

fn open_core(db_path: &Path) -> Result<Core<SqliteStore>, CliError> {
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
        TaskCommands::Ls => run_task_ls(db_path),
        TaskCommands::Edit(args) => run_task_edit(db_path, args),
        TaskCommands::Delete(args) => run_task_delete(db_path, &args),
        TaskCommands::Restore(args) => run_task_restore(db_path, &args),
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
    let new_task = NewTask {
        title: args.title,
        description: args.description,
        parent_ids: args.parent.into_iter().map(TaskId::from).collect(),
        type_key: None,
        start_date: args.start,
        due_date: args.due,
        assignee_id: args.assignee.map(UserId::from),
    };
    let task = core.create_task(new_task)?;
    println!("{}", Uuid::from(task.id));
    Ok(())
}

fn run_task_ls(db_path: &Path) -> Result<(), CliError> {
    let core = open_core(db_path)?;
    let tasks = core.get_tree(TreeFilter::default())?;
    let names: HashMap<UserId, String> = core
        .list_users()?
        .into_iter()
        .map(|user| (user.id, user.name))
        .collect();
    for task in &tasks {
        // Minimal nesting: indent one level under a task's first parent
        // (if any) so a child visibly nests under it — full recursive tree
        // layout is the TUI's job later.
        let indent = if task.parent_ids.is_empty() { "" } else { "  " };
        println!("{}", format_task_line(task, indent, &names));
    }
    Ok(())
}

/// Renders the one-line summary shared by `task ls` and `task edit`:
/// `"{indent}{id} {title}{assignee_suffix}"`.
fn format_task_line(task: &Task, indent: &str, names: &HashMap<UserId, String>) -> String {
    let assignee = task
        .assignee_id
        .and_then(|id| names.get(&id))
        .map(|name| format!(" (assigned: {name})"))
        .unwrap_or_default();
    format!("{indent}{} {}{assignee}", Uuid::from(task.id), task.title)
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
        let indent = if task.parent_ids.is_empty() { "" } else { "  " };
        println!("{}", format_task_line(task, indent, &names));
    }
    Ok(())
}

fn run_task_delete(db_path: &Path, args: &DeleteArgs) -> Result<(), CliError> {
    let mut core = open_core(db_path)?;
    let target_id = TaskId::from(args.id);

    // `Core` exposes no single-task lookup or "children of id" method, so
    // finding the target and its children both require scanning the full
    // tree — but one fetch is enough for both; the CLI-level inefficiency
    // this replaces was calling `get_tree` twice for the same snapshot.
    let mut tasks = core.get_tree(TreeFilter::default())?;
    let target_index = tasks
        .iter()
        .position(|task| task.id == target_id)
        .ok_or(CoreError::NotFound(target_id))?;
    let target = tasks.swap_remove(target_index);

    let children: Vec<Task> = tasks
        .into_iter()
        .filter(|task| task.parent_ids.contains(&target_id))
        .collect();

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
    let indent = if task.parent_ids.is_empty() { "" } else { "  " };
    println!("{}", format_task_line(&task, indent, &names));
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
