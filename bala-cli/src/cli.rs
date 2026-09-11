//! Command dispatch: builds a `Core<SqliteStore>` over the resolved db path
//! and drives it from parsed CLI arguments.

use std::path::{Path, PathBuf};

use bala_core::{Core, CoreError, NewTask, StoreError, TaskId, TreeFilter};
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
    /// Task operations: `add`, `ls`.
    Task(TaskArgs),
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
    };
    let task = core.create_task(new_task)?;
    println!("{}", Uuid::from(task.id));
    Ok(())
}

fn run_task_ls(db_path: &Path) -> Result<(), CliError> {
    let core = open_core(db_path)?;
    let tasks = core.get_tree(TreeFilter::default())?;
    for task in &tasks {
        // Minimal nesting: indent one level under a task's first parent
        // (if any) so a child visibly nests under it — full recursive tree
        // layout is the TUI's job later.
        let indent = if task.parent_ids.is_empty() { "" } else { "  " };
        println!("{indent}{} {}", Uuid::from(task.id), task.title);
    }
    Ok(())
}
