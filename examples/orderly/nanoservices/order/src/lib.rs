//! `order`: Drives an order to paid and fulfilled.
//!
//! Read `AGENTS.md` in this directory and `docs/DIRECTIVE.md` before editing.
//! What this nanoservice owns is what it is: 2 processing-object
//! type(s), 1 plain table(s), 1 external call(s).

// A rendered skeleton has stubs nothing calls yet; the allow goes once the
// bodies are filled.
#![allow(dead_code)]

use basable_core::AppError;
use basable_db::{NanoPool, Nanoservice, Stateful};

pub mod handlers;
pub mod effects;
pub mod provider;
pub mod simulator;
pub mod model;
pub mod repository;
pub mod worker;
pub mod types;

/// The schema marker: this nanoservice owns `nano_order` and gets a
/// pool bound to it (`SET ROLE nano_order`). A cross-schema query
/// from that pool is a Postgres permission error.
pub struct Schema;
impl Nanoservice for Schema {
    const NAME: &'static str = "order";
}
impl Stateful for Schema {}

/// The component. One value per process, shared by every handler and worker
/// (`&self` everywhere; in-memory state, if any, behind `std::sync::Mutex`
/// or atomics and never held across an `.await`).
pub struct Order {
    pub(crate) pool: NanoPool<Schema>,
    pub(crate) calls: effects::Calls,
    pub(crate) order_store: types::order::Store,
    pub(crate) shipment_store: types::shipment::Store,
}

impl Order {
    /// Builds the component. Called once from `app/src/main.rs`.
    /// Binding a type's store checks its migration ran; a missing one is a
    /// boot failure, which is the right time to find out.
    pub async fn new(_app: &basable_app::App, pool: NanoPool<Schema>) -> Self {
        Self {
            pool: pool.clone(),
            // Production wires the real provider; tests wire the simulator.
            calls: effects::Calls::from_env(),
            order_store: types::order::Store::bind(&pool, types::order::decl())
                .await
                .expect("order: bind type order (the migration must have run)"),
            shipment_store: types::shipment::Store::bind(&pool, types::shipment::decl())
                .await
                .expect("order: bind type shipment (the migration must have run)"),
        }
    }
}

/// The error a step not filled in yet answers with, so the skeleton deploys
/// green instead of panicking. `regex_search unimplemented_step` lists what
/// is left.
pub(crate) fn unimplemented_step(step: &'static str) -> AppError {
    tracing::warn!(step, "unimplemented step reached");
    AppError::unimplemented(step)
}
