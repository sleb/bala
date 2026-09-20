//! Configuration: resolving the default SQLite database path, and
//! loading/saving the persisted TUI `ViewState` (`view.toml`).

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use bala_core::TaskId;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Errors resolving or preparing a bala config/data directory.
// The shared "Dir" postfix pairs two orthogonal concepts (data vs. config
// dir; not-found vs. create-failed), not near-duplicate variants — keeping
// it is clearer than inventing dissimilar names to dodge the lint.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not determine a data directory for bala")]
    NoDataDir,

    #[error("failed to create data directory: {0}")]
    CreateDataDir(#[source] io::Error),

    #[error("could not determine a config directory for bala")]
    NoConfigDir,

    #[error("failed to create config directory: {0}")]
    CreateConfigDir(#[source] io::Error),
}

/// Resolves bala's `ProjectDirs`, the shared starting point for both the
/// data dir (`default_db_path`) and the config dir (`view_state_path`), so
/// the `ProjectDirs::from("", "", "bala")` call itself lives in exactly one
/// place.
fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "bala")
}

/// Resolves the default SQLite database path (per-OS data dir + `bala.db`),
/// creating the containing directory if it doesn't already exist.
///
/// # Errors
///
/// Returns `Err` if no data directory can be determined for this OS, or if
/// creating it fails.
pub fn default_db_path() -> Result<PathBuf, ConfigError> {
    let dirs = project_dirs().ok_or(ConfigError::NoDataDir)?;
    let data_dir = dirs.data_dir();
    std::fs::create_dir_all(data_dir).map_err(ConfigError::CreateDataDir)?;
    Ok(data_dir.join("bala.db"))
}

/// Resolves the path to the persisted TUI view state (per-OS config dir +
/// `view.toml`), creating the containing directory if it doesn't already
/// exist.
///
/// # Errors
///
/// Returns `Err` if no config directory can be determined for this OS, or if
/// creating it fails.
pub fn view_state_path() -> Result<PathBuf, ConfigError> {
    let dirs = project_dirs().ok_or(ConfigError::NoConfigDir)?;
    let config_dir = dirs.config_dir();
    std::fs::create_dir_all(config_dir).map_err(ConfigError::CreateConfigDir)?;
    Ok(config_dir.join("view.toml"))
}

/// Persisted TUI view state: which task (if any) is currently selected in
/// the tree, and which tasks are collapsed.
///
/// Other fields (`gantt_scale`, `gantt_anchor`, `filter`, `blocked_only`,
/// ...) described in the design doc are deliberately out of scope for this
/// story and may join later without a format break, since they'd live
/// alongside `selected` and `collapsed` inside the same `[tree]` table (or a
/// sibling table).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewState {
    pub selected: Option<TaskId>,
    pub collapsed: HashSet<TaskId>,
    /// The active TUI type filter (`Action::CycleTypeFilter`, bound to `f`),
    /// or `None` when no filter is applied.
    pub filter_type_key: Option<String>,
}

/// On-disk shape of `view.toml`. Kept private and separate from
/// [`ViewState`] so `TaskId` itself never needs to implement `serde`
/// traits — it's converted to/from `String` at this boundary via its
/// `Display`/`FromStr` impls.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ViewStateShape {
    tree: TreeShape,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TreeShape {
    selected: Option<String>,
    /// Missing from a `view.toml` saved before this field existed — default
    /// to empty rather than failing the whole document's deserialization
    /// (which would also discard the file's `selected`).
    #[serde(default)]
    collapsed: Vec<String>,
    /// Missing from a `view.toml` saved before this field existed — default
    /// to `None` rather than failing the whole document's deserialization,
    /// the same tolerance `collapsed` already has.
    #[serde(default)]
    filter_type_key: Option<String>,
}

impl From<&ViewState> for ViewStateShape {
    fn from(state: &ViewState) -> Self {
        Self {
            tree: TreeShape {
                selected: state.selected.map(|id| id.to_string()),
                collapsed: state.collapsed.iter().map(ToString::to_string).collect(),
                filter_type_key: state.filter_type_key.clone(),
            },
        }
    }
}

impl From<ViewStateShape> for ViewState {
    fn from(shape: ViewStateShape) -> Self {
        Self {
            selected: shape.tree.selected.and_then(|s| s.parse().ok()),
            collapsed: shape
                .tree
                .collapsed
                .into_iter()
                .filter_map(|s| s.parse().ok())
                .collect(),
            filter_type_key: shape.tree.filter_type_key,
        }
    }
}

/// Loads the persisted view state from `path`.
///
/// A missing file, an unreadable file, or a file whose contents don't parse
/// as valid `view.toml` all fall back to [`ViewState::default`] rather than
/// surfacing an error — persisted view state is a convenience, never a
/// reason to block startup.
#[must_use]
pub fn load_view_state(path: &Path) -> ViewState {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return ViewState::default();
    };
    toml::from_str::<ViewStateShape>(&contents).map_or_else(|_| ViewState::default(), Into::into)
}

/// Saves `state` to `path`, replacing any existing file.
///
/// The new contents are written to a sibling temp file first and then
/// renamed over `path`, so a crash or power loss mid-write can never leave
/// `path` holding a partially-written (corrupt) file.
///
/// # Errors
///
/// Returns `Err` if serialization fails, or if writing or renaming the temp
/// file fails.
pub fn save_view_state(path: &Path, state: &ViewState) -> io::Result<()> {
    let shape = ViewStateShape::from(state);
    let contents = toml::to_string_pretty(&shape).map_err(io::Error::other)?;

    let temp_path = path.with_extension("toml.tmp");
    std::fs::write(&temp_path, contents)?;
    std::fs::rename(&temp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::str::FromStr as _;

    #[test]
    fn view_state_should_round_trip_through_toml() {
        let id = TaskId::from_str("00000000-0000-0000-0000-000000000001").expect("valid uuid");
        let collapsed_a =
            TaskId::from_str("00000000-0000-0000-0000-000000000002").expect("valid uuid");
        let collapsed_b =
            TaskId::from_str("00000000-0000-0000-0000-000000000003").expect("valid uuid");
        let state = ViewState {
            selected: Some(id),
            collapsed: [collapsed_a, collapsed_b].into_iter().collect(),
            filter_type_key: Some("goal".to_string()),
        };

        let shape = ViewStateShape::from(&state);
        let toml_text = toml::to_string_pretty(&shape).expect("serialize");
        let parsed: ViewStateShape = toml::from_str(&toml_text).expect("parse");
        let round_tripped: ViewState = parsed.into();

        assert_eq!(round_tripped, state);
    }

    #[test]
    fn load_view_state_should_skip_unparseable_collapsed_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("view.toml");
        fs::write(
            &path,
            "[tree]\ncollapsed = [\"00000000-0000-0000-0000-000000000001\", \"not-a-uuid\"]\n",
        )
        .expect("write view.toml");

        let state = load_view_state(&path);

        let valid_id =
            TaskId::from_str("00000000-0000-0000-0000-000000000001").expect("valid uuid");
        assert_eq!(state.collapsed, [valid_id].into_iter().collect());
    }

    #[test]
    fn load_view_state_should_default_collapsed_when_file_predates_the_field() {
        // Regression: a `view.toml` written before `collapsed` existed has
        // only `selected` under `[tree]`. Without `#[serde(default)]` on
        // `TreeShape::collapsed`, that's a missing-field deserialize error,
        // which `load_view_state` downgrades to `ViewState::default()` —
        // silently discarding the user's saved `selected` too.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("view.toml");
        fs::write(
            &path,
            "[tree]\nselected = \"00000000-0000-0000-0000-000000000001\"\n",
        )
        .expect("write pre-collapsed view.toml");

        let state = load_view_state(&path);

        let selected_id =
            TaskId::from_str("00000000-0000-0000-0000-000000000001").expect("valid uuid");
        assert_eq!(state.selected, Some(selected_id));
        assert_eq!(state.collapsed, std::collections::HashSet::new());
    }

    #[test]
    fn save_view_state_should_overwrite_existing_collapsed_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("view.toml");
        let old_id = TaskId::from_str("00000000-0000-0000-0000-000000000001").expect("valid uuid");
        save_view_state(
            &path,
            &ViewState {
                selected: None,
                collapsed: [old_id].into_iter().collect(),
                filter_type_key: None,
            },
        )
        .expect("save old state");

        let new_id = TaskId::from_str("00000000-0000-0000-0000-000000000002").expect("valid uuid");
        save_view_state(
            &path,
            &ViewState {
                selected: None,
                collapsed: [new_id].into_iter().collect(),
                filter_type_key: None,
            },
        )
        .expect("save new state");

        let loaded = load_view_state(&path);
        assert_eq!(
            loaded,
            ViewState {
                selected: None,
                collapsed: [new_id].into_iter().collect(),
                filter_type_key: None,
            }
        );
    }

    #[test]
    fn load_view_state_should_default_filter_type_key_when_file_predates_the_field() {
        // Regression, same shape as `collapsed`'s predates-the-field test:
        // a `view.toml` written before `filter_type_key` existed has only
        // `selected` under `[tree]`. Without `#[serde(default)]` on
        // `TreeShape::filter_type_key`, that's a missing-field deserialize
        // error, which `load_view_state` downgrades to
        // `ViewState::default()` — silently discarding the user's saved
        // `selected` too.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("view.toml");
        fs::write(
            &path,
            "[tree]\nselected = \"00000000-0000-0000-0000-000000000001\"\n",
        )
        .expect("write pre-filter_type_key view.toml");

        let state = load_view_state(&path);

        let selected_id =
            TaskId::from_str("00000000-0000-0000-0000-000000000001").expect("valid uuid");
        assert_eq!(state.selected, Some(selected_id));
        assert_eq!(state.filter_type_key, None);
    }

    #[test]
    fn load_view_state_should_return_default_when_file_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("view.toml");

        let state = load_view_state(&path);

        assert_eq!(state, ViewState::default());
    }

    #[test]
    fn load_view_state_should_return_default_when_file_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("view.toml");
        fs::write(&path, "this is not valid toml [[[").expect("write corrupt file");

        let state = load_view_state(&path);

        assert_eq!(state, ViewState::default());
    }

    #[test]
    fn save_view_state_should_overwrite_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("view.toml");
        let old_id = TaskId::from_str("00000000-0000-0000-0000-000000000001").expect("valid uuid");
        save_view_state(
            &path,
            &ViewState {
                selected: Some(old_id),
                collapsed: HashSet::new(),
                filter_type_key: None,
            },
        )
        .expect("save old state");

        let new_id = TaskId::from_str("00000000-0000-0000-0000-000000000002").expect("valid uuid");
        save_view_state(
            &path,
            &ViewState {
                selected: Some(new_id),
                collapsed: HashSet::new(),
                filter_type_key: None,
            },
        )
        .expect("save new state");

        let loaded = load_view_state(&path);
        assert_eq!(
            loaded,
            ViewState {
                selected: Some(new_id),
                collapsed: HashSet::new(),
                filter_type_key: None,
            }
        );
    }

    #[test]
    fn save_view_state_should_leave_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("view.toml");

        save_view_state(&path, &ViewState::default()).expect("save state");

        let entries: Vec<_> = fs::read_dir(dir.path())
            .expect("read dir")
            .map(|entry| entry.expect("dir entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("view.toml")]);
    }
}
