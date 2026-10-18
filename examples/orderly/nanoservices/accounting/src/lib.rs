//! `accounting`: Books orders and payments on the ledger.
//!
//! Read `AGENTS.md` in this directory and `docs/DIRECTIVE.md` before editing.
//! What this nanoservice owns is what it is: 0 processing-object
//! type(s), 0 plain table(s), 0 external call(s), and
//! the project's ledger (2 object type(s)).

// A rendered skeleton has stubs nothing calls yet; the allow goes once the
// bodies are filled.
#![allow(dead_code)]

use basable_core::AppError;
use basable_db::{NanoPool, Nanoservice, Stateful};

pub mod handlers;
pub mod ledger;

/// The schema marker: this nanoservice owns `nano_accounting` and gets a
/// pool bound to it (`SET ROLE nano_accounting`). A cross-schema query
/// from that pool is a Postgres permission error.
pub struct Schema;
impl Nanoservice for Schema {
    const NAME: &'static str = "accounting";
}
impl Stateful for Schema {}

/// The component. One value per process, shared by every handler and worker
/// (`&self` everywhere; in-memory state, if any, behind `std::sync::Mutex`
/// or atomics and never held across an `.await`).
pub struct Accounting {
    pub(crate) pool: NanoPool<Schema>,
    pub(crate) ledger: ledger::AppLedger,
}

impl Accounting {
    /// Builds the component. Called once from `app/src/main.rs`.
    /// Building the ledger restates every process behind the running rules
    /// before anything serves (`ledger::build`).
    pub async fn new(_app: &basable_app::App, pool: NanoPool<Schema>) -> Self {
        Self {
            pool: pool.clone(),
            ledger: ledger::build(pool.pool()).await,
        }
    }
}

/// This nanoservice runs no loop of its own (no processing-object type, no
/// schedule): it answers messages. A type or a schedule it gains brings a
/// `loops()` here.
impl<R: 'static> basable_app::Component<R> for Accounting {}

/// The error a step not filled in yet answers with, so the skeleton deploys
/// green instead of panicking. `regex_search unimplemented_step` lists what
/// is left.
pub(crate) fn unimplemented_step(step: &'static str) -> AppError {
    tracing::warn!(step, "unimplemented step reached");
    AppError::unimplemented(step)
}
