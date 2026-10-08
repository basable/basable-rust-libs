//! The sends-only boundary: it holds nothing and sends through the router
//! it is handed per call.

use basable_app::Component;
use basable_core::{AppError, Ctx};
use interfaces::{ApiRoutes, ApiSender};
use messages::*;

#[derive(Debug, Default, Clone, Copy)]
pub struct Api;

impl Api {
    pub fn new() -> Self {
        Api
    }

    pub async fn ensure_order<R: ApiRoutes>(
        &self,
        router: &R,
        ctx: &Ctx,
        product: &str,
    ) -> Result<Order, AppError> {
        ApiSender::new(router)
            .send_ensure_order_request(
                ctx,
                EnsureOrderRequest {
                    product: product.to_string(),
                },
            )
            .await
    }
}

/// Sends-only and no loops: the default.
impl<R> Component<R> for Api {}
