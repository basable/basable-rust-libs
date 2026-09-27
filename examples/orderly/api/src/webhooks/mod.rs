//! Inbound webhooks: raw axum routes beside the Connect router. Each module
//! verifies its provider's signature, then makes ONE messenger send to the
//! owning nanoservice. The scaffolder adds `pub mod <provider>;` per declared
//! webhook between the markers.

use axum::Router;

use crate::Api;

// basable:webhooks-begin
// basable:webhooks-end

/// Every webhook route, mounted under `/api/webhooks/<provider>`.
pub fn routes<R>(_api: &'static Api, _router: &'static R) -> Router
where
    R: interfaces::ApiRoutes + Send + Sync + 'static,
{
    let router = Router::new();
    // basable:webhook-routes-begin
    // basable:webhook-routes-end
    router
}
