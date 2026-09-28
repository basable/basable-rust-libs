//! Schedules are WORKERS this nanoservice owns: a ticker registered with
//! the app (app/src/main.rs), immediate first tick then one per interval,
//! failures logged and retried next tick, joined on shutdown. Never a
//! CronJob. A tick that needs claim authority creates or nudges a
//! processing object; it never writes status itself.

use basable_app::Ticker;
use basable_core::{BoxError, Ctx};

use crate::Order;

impl Order {
    /// `sweep_abandoned`, every 15m.
    pub async fn tick_sweep_abandoned(&self, _ctx: &Ctx) -> Result<(), BoxError> {
        Err(crate::unimplemented_step("order.tick_sweep_abandoned").into())
    }
}

/// The tickers `main.rs` registers for this nanoservice. The router is here
/// for the tick that sends.
pub fn tickers<R>(_router: &'static R, this: &'static Order) -> Vec<Ticker>
where
    R: interfaces::OrderRoutes + Send + Sync + 'static,
{
    vec![
        Ticker::new("order.sweep_abandoned", std::time::Duration::from_secs(900), move |ctx| Box::pin(this.tick_sweep_abandoned(ctx))),
    ]
}
