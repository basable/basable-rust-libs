//! The handler trait implementation: one method per message this
//! nanoservice handles (routing.yaml). Handlers are the INTENT boundary:
//! they validate, write desired state through the typed store (`create`,
//! `update_spec`, `mark_deleted`, `nudge`) or the repository, dispatch
//! request-path adapters `Unfenced`, and send through `s` (the sender that
//! exposes exactly the declared sends). They never write status.

use basable_core::{AppError, Ctx};
use interfaces::{OrderHandler, OrderRoutes, OrderSender};

use crate::Order;

// Messages stay qualified: a message may share its name with this
// component (an `order` nanoservice handling an `Order` response).
impl<R: OrderRoutes> OrderHandler<R> for Order {
    async fn handle_ensure_order_request(&self, _ctx: &Ctx, _msg: messages::EnsureOrderRequest, _s: OrderSender<'_, R>) -> Result<messages::Order, AppError> {
        Err(crate::unimplemented_step("order.handle_ensure_order_request"))
    }
}
