//! A stub `order`: an ensure asks the catalog, notifies, and emits the
//! event; a cancel fails.

use std::sync::atomic::{AtomicU64, Ordering};

use basable_core::{AppError, Ctx};
use interfaces::{OrderHandler, OrderRoutes, OrderSender};
use messages::*;

#[derive(Default)]
pub struct Order {
    next_id: AtomicU64,
}

impl Order {
    pub fn new() -> Self {
        Order::default()
    }
}

impl<R: OrderRoutes> OrderHandler<R> for Order {
    async fn handle_ensure_order_request(
        &self,
        ctx: &Ctx,
        msg: EnsureOrderRequest,
        s: OrderSender<'_, R>,
    ) -> Result<messages::Order, AppError> {
        let product = s
            .send_get_product_request(
                ctx,
                GetProductRequest {
                    name: msg.product.clone(),
                },
            )
            .await?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let receipt = s
            .send_send_notification_request(
                ctx,
                SendNotificationRequest {
                    text: format!("order {id} for {}", product.name),
                },
            )
            .await?;
        assert!(receipt.text.contains(&product.name));
        s.send_order_event(ctx, OrderEvent { order: id }).await;
        Ok(messages::Order {
            id,
            product: product.name,
        })
    }

    async fn handle_cancel_order_request(
        &self,
        _ctx: &Ctx,
        msg: CancelOrderRequest,
        _s: OrderSender<'_, R>,
    ) -> Result<messages::Order, AppError> {
        Err(AppError::not_found(format!(
            "order {} cannot be cancelled",
            msg.order
        )))
    }
}
