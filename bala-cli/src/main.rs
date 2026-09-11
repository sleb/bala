//! `bala` CLI entry point: parses argv and dispatches to the `cli` module.
//! No subcommand launches a real TUI yet — that's later work; running
//! `bala` alone just prints a placeholder message.

mod cli;
mod config;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Commands};

fn main() -> ExitCode {
    let args = Cli::parse();

    let Some(command) = args.command else {
        println!("No subcommand given. Run `bala --help` for usage.");
        return ExitCode::SUCCESS;
    };

    let db_path = match resolve_db_path(args.db_path) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let result = match command {
        Commands::Task(task_args) => cli::run_task_command(&db_path, task_args.command),
        Commands::User(user_args) => cli::run_user_command(&db_path, user_args.command),
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
