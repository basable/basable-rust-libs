//! The Connect boundary of the application. `Api` is a sends-only
//! nanoservice: it implements the connect-rust service traits generated from
//! `proto/`, validates each request, converts the buffa request into the
//! plain `messages` struct, sends it through its generated sender, converts
//! the answer back, and maps `AppError` to a Connect code
//! (`basable_connect`). Raw webhook routes live beside the Connect router in
//! `webhooks/` and verify the provider's signature before ONE messenger send.
//!
//! Kratos sessions are validated by `basable_auth`'s tower layer mounted in
//! `app/src/main.rs`; a handler reads the caller from `ctx.user_id()`.

// A rendered skeleton has stubs nothing calls yet; the allow goes once the
// bodies are filled.
#![allow(dead_code)]

use basable_connect::ConnectRouter;
use basable_core::AppError;

pub mod services;
pub mod webhooks;

/// The sends-only component. It holds nothing: each Connect service and
/// webhook route builds its `ApiSender` once, at mount, from the `&'static`
/// router `main.rs` hands in, and sends through that.
#[derive(Debug, Default, Clone, Copy)]
pub struct Api;

/// The API runs no loop of its own: it serves requests. (Every component
/// implements `Component`; the app starts the loops of all of them.)
impl<R: 'static> basable_app::Component<R> for Api {}

impl Api {
    pub fn new() -> Self {
        Api
    }

    /// Mounts every generated Connect service onto one router. Services are
    /// registered in `services/mod.rs` by the scaffolder, one per
    /// nanoservice that declares an API.
    pub fn connect_router<R>(&'static self, router: &'static R) -> ConnectRouter
    where
        R: interfaces::ApiRoutes + Send + Sync + 'static,
    {
        let mut connect = ConnectRouter::new();
        services::mount_all(&mut connect, self, router);
        connect
    }

    /// The raw (non-Connect) routes: inbound webhooks.
    pub fn raw_routes<R>(&'static self, router: &'static R) -> axum::Router
    where
        R: interfaces::ApiRoutes + Send + Sync + 'static,
    {
        webhooks::routes(self, router)
    }
}

/// The error a step the agent has not filled in yet answers with, so the
/// skeleton deploys green instead of panicking. `regex_search
/// unimplemented_step` lists what is left.
pub(crate) fn unimplemented_step(step: &'static str) -> AppError {
    tracing::warn!(step, "unimplemented step reached");
    AppError::unimplemented(step)
}
