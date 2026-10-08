//! `notifier`: Sends one templated email per event.
//!
//! Read `AGENTS.md` in this directory and `docs/DIRECTIVE.md` before editing.
//! What this nanoservice owns is what it is: 0 processing-object
//! type(s), 0 plain table(s), 1 external call(s).

// A rendered skeleton has stubs nothing calls yet; the allow goes once the
// bodies are filled.
#![allow(dead_code)]

use basable_core::AppError;

pub mod handlers;
pub mod effects;
pub mod provider;
pub mod simulator;

/// The component. One value per process, shared by every handler and worker
/// (`&self` everywhere; in-memory state, if any, behind `std::sync::Mutex`
/// or atomics and never held across an `.await`).
pub struct Notifier {
    pub(crate) calls: effects::Calls,
}

impl Notifier {
    /// Builds the component. Called once from `app/src/main.rs`.
    pub fn new(_app: &basable_app::App) -> Self {
        Self {
            // Production wires the real provider; tests wire the simulator.
            calls: effects::Calls::from_env(),
        }
    }
}

/// This nanoservice runs no loop of its own (no processing-object type, no
/// schedule): it answers messages. A type or a schedule it gains brings a
/// `loops()` here.
impl<R: 'static> basable_app::Component<R> for Notifier {}

/// The error a step not filled in yet answers with, so the skeleton deploys
/// green instead of panicking. `regex_search unimplemented_step` lists what
/// is left.
pub(crate) fn unimplemented_step(step: &'static str) -> AppError {
    tracing::warn!(step, "unimplemented step reached");
    AppError::unimplemented(step)
}
