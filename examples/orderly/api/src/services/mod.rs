//! One module per nanoservice that exposes a Connect service. The scaffolder
//! adds `pub mod <name>;` between the markers and each module's `mount`
//! registers its service; the agent fills the field mapping inside.

use basable_connect::ConnectRouter;

use crate::Api;

// basable:services-begin
pub mod catalog;
pub mod order;
// basable:services-end

/// Mounts every service module's Connect service.
pub fn mount_all<R>(connect: &mut ConnectRouter, api: &'static Api, router: &'static R)
where
    R: interfaces::ApiRoutes + Send + Sync + 'static,
{
    health::mount(connect, api, router);
    // basable:mount-begin
    catalog::mount(connect, api, router);
    order::mount(connect, api, router);
    // basable:mount-end
}

/// The health service: the Connect face of `/readyz`, answered once the
/// app finished wiring (a request cannot arrive before that).
pub mod health {
    use std::sync::Arc;

    use basable_connect::ConnectRouter;
    use connectrpc::{RequestContext, Response, ServiceRequest, ServiceResult};
    use proto::connect::health::v1::HealthService;
    use proto::proto::health::v1::{CheckRequest, CheckResponse};

    use crate::Api;

    pub struct Health;

    #[allow(refining_impl_trait)]
    impl HealthService for Health {
        async fn check(&self, _ctx: RequestContext, _request: ServiceRequest<'_, CheckRequest>) -> ServiceResult<CheckResponse> {
            Ok(Response::new(CheckResponse {
                ready: true,
                ..Default::default()
            }))
        }
    }

    pub fn mount<R>(connect: &mut ConnectRouter, _api: &'static Api, _router: &'static R)
    where
        R: interfaces::ApiRoutes + Send + Sync + 'static,
    {
        connect.add_service(Arc::new(Health));
    }
}
