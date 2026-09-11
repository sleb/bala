//! Minimal configuration: resolving the default SQLite database path.
//!
//! No `view.toml`/`ViewState` at this checkpoint — just enough to find (and
//! create) a directory to put `bala.db` in.

use std::io;
use std::path::PathBuf;

use directories::ProjectDirs;

/// Errors resolving or preparing the default db path.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not determine a data directory for bala")]
    NoDataDir,

    #[error("failed to create data directory: {0}")]
    CreateDataDir(#[source] io::Error),
}

/// Resolves the default SQLite database path (per-OS data dir + `bala.db`),
/// creating the containing directory if it doesn't already exist.
///
/// # Errors
///
/// Returns `Err` if no data directory can be determined for this OS, or if
/// creating it fails.
pub fn default_db_path() -> Result<PathBuf, ConfigError> {
    let dirs = ProjectDirs::from("", "", "bala").ok_or(ConfigError::NoDataDir)?;
    let data_dir = dirs.data_dir();
    std::fs::create_dir_all(data_dir).map_err(ConfigError::CreateDataDir)?;
    Ok(data_dir.join("bala.db"))
}
