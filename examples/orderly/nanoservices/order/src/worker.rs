//! Schedules are WORKERS this nanoservice owns: a ticker in its `loops()`
//! (lib.rs), which the app starts and joins on shutdown — immediate first
//! tick then one per interval, failures logged and retried next tick.
//! Never a CronJob. A tick that needs claim authority creates or nudges a
//! processing object; it never writes status itself.

use basable_core::{BoxError, Ctx};

use crate::Order;

impl Order {
    /// `sweep_abandoned`, every 15m. `_s` is this nanoservice's sender,
    /// for a tick that sends.
    pub async fn tick_sweep_abandoned<R>(&self, _ctx: &Ctx, _s: interfaces::OrderSender<'static, R>) -> Result<(), BoxError>
    where
        R: interfaces::OrderRoutes + Send + Sync + 'static,
    {
        Err(crate::unimplemented_step("order.tick_sweep_abandoned").into())
    }
}
