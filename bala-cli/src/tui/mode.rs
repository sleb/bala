//! Interaction mode for the TUI.
//!
//! Only `Normal` exists yet: browsing the task list has one mode, so
//! `handle_key` doesn't need a `Mode` parameter today, and nothing
//! constructs a `Mode` yet either. Later stories (insert/reschedule/
//! confirm/filter/help) add variants here, give `App` a `mode: Mode` field,
//! and thread a `&mut Mode` through `handle_key` once dispatch actually
//! depends on it. Defined now (per LLD-3, ahead of that wiring) so this
//! story's own issue text — which names `Mode` as this checkpoint's
//! production output — has a real type for later stories to extend instead
//! of introducing it from scratch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "scaffolding for later stories' mode variants; not constructed until App gains a mode field"
)]
pub enum Mode {
    Normal,
}
