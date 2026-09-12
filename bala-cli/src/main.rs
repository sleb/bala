//! `bala` CLI entry point: parses argv and dispatches to either the
//! interactive TUI (no subcommand given) or the `cli` module's subcommand
//! handlers.

mod cli;
mod config;
mod render;
mod tui;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Commands};

fn main() -> ExitCode {
    let args = Cli::parse();

    let db_path = match resolve_db_path(args.db_path) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let result = match args.command {
        None => tui::run(&db_path),
        Some(Commands::Task(task_args)) => cli::run_task_command(&db_path, task_args.command),
        Some(Commands::User(user_args)) => cli::run_user_command(&db_path, user_args.command),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn resolve_db_path(override_path: Option<PathBuf>) -> Result<PathBuf, config::ConfigError> {
    match override_path {
        Some(path) => Ok(path),
        None => config::default_db_path(),
    }
}
