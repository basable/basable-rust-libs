//! `OrderService`: the Connect service of `order`. Each
//! method validates the request, converts the buffa message into the plain
//! `messages` struct, sends it through the api sender, and converts the
//! answer back. Errors map to Connect codes through basable_connect.

use std::sync::Arc;

use basable_connect::{ConnectRouter, into_connect_error, request_ctx};
use connectrpc::{RequestContext, ServiceRequest, ServiceResult};
use interfaces::ApiRoutes;
use proto::connect::order::v1::OrderService;
use proto::proto::order::v1::*;

use crate::Api;

pub struct OrderServiceImpl<R: ApiRoutes + Send + Sync + 'static> {
    router: &'static R,
}

// The generated trait returns `impl Encodable`; answering with the owned
// message is the refinement this allow names.
#[allow(refining_impl_trait)]
impl<R: ApiRoutes + Send + Sync + 'static> OrderService for OrderServiceImpl<R> {
    async fn ensure_order(&self, ctx: RequestContext, request: ServiceRequest<'_, EnsureOrderRequest>) -> ServiceResult<EnsureOrderResponse> {
        let ctx = request_ctx(&ctx);
        let _req = request.to_owned_message();
        // TODO: validate `_req`, convert it into `messages::EnsureOrderRequest`,
        // send it with `interfaces::ApiSender::new(self.router).send_…(&ctx, m).await`,
        // and convert the answer into `EnsureOrderResponse` (`Ok(connectrpc::Response::new(..))`).
        let _ = (&ctx, self.router);
        Err(into_connect_error(crate::unimplemented_step("api.ensure_order")))
    }
}

pub fn mount<R>(connect: &mut ConnectRouter, _api: &'static Api, router: &'static R)
where
    R: ApiRoutes + Send + Sync + 'static,
{
    connect.add_service(Arc::new(OrderServiceImpl { router }));
}
