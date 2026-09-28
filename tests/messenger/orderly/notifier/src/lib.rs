//! A stub `notifier`: acknowledges notifications and records events.

use std::sync::{Arc, Mutex};

use basable_core::{AppError, Ctx};
use interfaces::{NotifierHandler, NotifierRoutes, NotifierSender};
use messages::*;

pub struct Notifier {
    trace: Arc<Mutex<Vec<String>>>,
}

impl Notifier {
    pub fn new(trace: Arc<Mutex<Vec<String>>>) -> Self {
        Notifier { trace }
    }
}

impl<R: NotifierRoutes> NotifierHandler<R> for Notifier {
    async fn handle_send_notification_request(
        &self,
        _ctx: &Ctx,
        msg: SendNotificationRequest,
        _s: NotifierSender<'_, R>,
    ) -> Result<NotificationReceipt, AppError> {
        self.trace
            .lock()
            .unwrap()
            .push(format!("notifier:send:{}", msg.text));
        Ok(NotificationReceipt { text: msg.text })
    }

    async fn handle_order_event(&self, _ctx: &Ctx, msg: OrderEvent, _s: NotifierSender<'_, R>) {
        self.trace
            .lock()
            .unwrap()
            .push(format!("notifier:event:{}", msg.order));
    }
}
