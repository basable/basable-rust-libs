//! The handler trait implementation: one method per message this
//! nanoservice handles (routing.yaml). Handlers are the INTENT boundary:
//! they validate, write desired state through the typed store (`create`,
//! `update_spec`, `mark_deleted`, `nudge`) or the repository, dispatch
//! request-path adapters `Unfenced`, and send through `s` (the sender that
//! exposes exactly the declared sends). They never write status.

use basable_core::Ctx;
use interfaces::{NotifierHandler, NotifierRoutes, NotifierSender};

use crate::Notifier;

// Messages stay qualified: a message may share its name with this
// component (an `order` nanoservice handling an `Order` response).
impl<R: NotifierRoutes> NotifierHandler<R> for Notifier {
    async fn handle_order_event(&self, _ctx: &Ctx, _msg: messages::OrderEvent, _s: NotifierSender<'_, R>) {
        // A void fan-out event: observe it, nudge an object or write a side
        // table owned by this nanoservice; never a status write.
        tracing::warn!(step = "notifier.handle_order_event", "unimplemented step reached");
    }
}
