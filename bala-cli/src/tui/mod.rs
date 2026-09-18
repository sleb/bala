//! TUI implementation modules.
//!
//! `app` holds the pure selection-state model; `screens` renders it to a
//! ratatui frame; `mode` defines the (currently single-variant) interaction
//! mode; `run` is the crossterm-backed event loop that ties them together.

pub(crate) mod app;
pub(crate) mod keymap;
pub(crate) mod mode;
pub(crate) mod screens;

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use bala_core::TreeFilter;
use crossterm::event::{Event, KeyEventKind};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::cli::{self, CliError};
use crate::config::{self, ViewState};
use crate::render;
use app::{App, handle_key};

/// Runs the interactive TUI against the store at `db_path`.
///
/// Fetches the task tree, users, and task types once up front, then loops
/// drawing the current state and handling key input until the user quits
/// (`q`). Raw mode and the alternate screen are entered before the loop and
/// restored (via [`TerminalGuard`]'s `Drop`, and a panic hook installed
/// alongside it) on every exit path, including a panic mid-render.
///
/// # Errors
///
/// Returns `Err` if stdin/stdout aren't real terminals, the store can't be
/// opened, a `Core` call fails, or a terminal I/O operation (setup, draw,
/// or input polling) fails.
pub fn run(db_path: &Path) -> Result<(), CliError> {
    // `enable_raw_mode()` only configures input handling; it neither checks
    // nor fails on a redirected stdout. Without this check, a real-terminal
    // stdin paired with a redirected stdout (e.g. `bala > out.txt` from an
    // interactive shell) would enter raw mode successfully, write
    // alternate-screen/frame control sequences into the redirected file,
    // and sit in the event loop waiting on real keyboard input — corrupting
    // the file instead of failing cleanly. Checked first, before anything
    // else in `run`, since it's cheap and independent of the store.
    ensure_is_terminal(&std::io::stdin(), "stdin")?;
    ensure_is_terminal(&std::io::stdout(), "stdout")?;

    let mut core = cli::open_core(db_path)?;
    let users = core.list_users()?;
    let types = core.list_task_types()?;

    // Resolving the view-state path can fail only for a rare OS-level reason
    // (no config directory determinable, or it can't be created) — that's
    // not a reason to block the TUI from starting, so it's downgraded to a
    // stderr warning and treated as "no usable path", same as `run` treats
    // any other config-loading hiccup: `view_state_path` itself is `None`
    // hereafter, which also means the closing save step is skipped rather
    // than attempted against a path we couldn't resolve.
    let view_state_path = match config::view_state_path() {
        Ok(path) => Some(path),
        Err(err) => {
            eprintln!("warning: failed to resolve view state path: {err}");
            None
        }
    };
    let view_state = view_state_path
        .as_deref()
        .map(config::load_view_state)
        .unwrap_or_default();

    // A restored `filter_type_key` is honored from the first frame, not just
    // after the next `f` press — matching `refresh_rows_from_tree`'s
    // behavior once the TUI is running.
    let tasks = core.get_tree(TreeFilter {
        type_key: view_state.filter_type_key.clone(),
        ..Default::default()
    })?;
    let available_type_keys: Vec<String> = types
        .iter()
        .map(|task_type| task_type.key.clone())
        .collect();

    let user_names: HashMap<_, _> = users.into_iter().map(|user| (user.id, user.name)).collect();
    let type_labels: HashMap<_, _> = types
        .into_iter()
        .map(|task_type| (task_type.key, task_type.label))
        .collect();
    let descriptions: HashMap<_, _> = tasks
        .iter()
        .map(|task| (task.id, task.description.clone()))
        .collect();
    let rows = render::task_rows(
        &tasks,
        &type_labels,
        &user_names,
        &std::collections::HashSet::new(),
    );

    let mut app = App::new(rows)
        .with_lookup_maps(type_labels, user_names)
        .with_descriptions(descriptions)
        .with_tasks(tasks)
        .with_collapsed(view_state.collapsed)
        .with_type_filter_state(view_state.filter_type_key, available_type_keys);
    app.select_by_id(view_state.selected);

    install_panic_hook();
    crossterm::terminal::enable_raw_mode().map_err(CliError::TerminalIo)?;
    // From here on, `_guard`'s `Drop` restores the terminal on every exit
    // path (clean quit, an error propagated via `?` below, or an early
    // return), so it's constructed immediately after the first step that
    // needs undoing.
    let _guard = TerminalGuard;
    crossterm::execute!(std::io::stdout(), EnterAlternateScreen).map_err(CliError::TerminalIo)?;

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend).map_err(CliError::TerminalIo)?;

    loop {
        terminal
            .draw(|frame| screens::draw(frame, &app))
            .map_err(CliError::TerminalIo)?;

        // A short poll timeout (rather than a blocking `read()`) keeps the
        // loop responsive to future needs (e.g. a tick-driven refresh)
        // without busy-waiting; 100ms is imperceptible for a key press.
        if crossterm::event::poll(Duration::from_millis(100)).map_err(CliError::TerminalIo)? {
            let event = crossterm::event::read().map_err(CliError::TerminalIo)?;
            if let Event::Key(key) = event
                && key.kind == KeyEventKind::Press
                && handle_key(&mut app, &mut core, key).is_break()
            {
                break;
            }
        }
    }

    if let Some(path) = view_state_path.as_deref() {
        let new_state = ViewState {
            selected: app.selected_row().map(|row| row.id),
            collapsed: app.collapsed().clone(),
            filter_type_key: app.type_filter().map(str::to_owned),
        };
        if let Err(err) = config::save_view_state(path, &new_state) {
            eprintln!("warning: failed to save view state: {err}");
        }
    }

    Ok(())
}

/// Answers whether a stream is a real terminal. A thin, unsealed mirror of
/// `std::io::IsTerminal` (which can't be implemented for a test fake, being
/// sealed to `std`'s own types) so `ensure_is_terminal` can be exercised in
/// tests without a real terminal or a real non-terminal file descriptor.
trait TerminalCheck {
    fn is_a_terminal(&self) -> bool;
}

impl<T: IsTerminal> TerminalCheck for T {
    fn is_a_terminal(&self) -> bool {
        self.is_terminal()
    }
}

/// Fails with [`CliError::TerminalIo`] unless `stream` is a real terminal —
/// the shared check behind both the stdin and stdout guards in [`run`].
fn ensure_is_terminal(stream: &impl TerminalCheck, name: &str) -> Result<(), CliError> {
    if stream.is_a_terminal() {
        Ok(())
    } else {
        Err(CliError::TerminalIo(std::io::Error::other(format!(
            "{name} is not a terminal; the interactive TUI requires a real terminal"
        ))))
    }
}

/// Restores the terminal (raw mode + alternate screen) on drop, so it's
/// restored on every exit path from `run`: clean quit, an error propagated
/// via `?`, or an early return. Errors from the restoration itself are
/// swallowed (a `Drop` impl can't propagate `Result`, and there's no
/// terminal left to usefully report to at that point).
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Best-effort terminal restoration: disables raw mode and leaves the
/// alternate screen, ignoring failures.
fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
}

/// Installs a panic hook that restores the terminal before delegating to
/// the previously installed hook, so a panic mid-render doesn't leave the
/// user's terminal corrupted (raw mode left on, stuck in the alternate
/// screen).
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::{CliError, TerminalCheck, ensure_is_terminal};

    /// A fake stream whose answer is fixed at construction, so
    /// `ensure_is_terminal`'s branches can be tested without a real
    /// terminal or a real non-terminal file descriptor.
    struct FakeStream(bool);

    impl TerminalCheck for FakeStream {
        fn is_a_terminal(&self) -> bool {
            self.0
        }
    }

    #[test]
    fn ensure_is_terminal_should_succeed_when_stream_is_a_terminal() {
        assert!(ensure_is_terminal(&FakeStream(true), "stdin").is_ok());
    }

    #[test]
    fn ensure_is_terminal_should_fail_when_stream_is_not_a_terminal() {
        let err = ensure_is_terminal(&FakeStream(false), "stdout").unwrap_err();
        assert!(matches!(err, CliError::TerminalIo(_)));
        assert!(err.to_string().contains("stdout is not a terminal"));
    }
}
