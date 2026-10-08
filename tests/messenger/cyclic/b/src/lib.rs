//! `b`: answers a Pong by asking `a` again (a Ping).

use std::sync::{Arc, Mutex};

use basable_app::Component;
use basable_core::{AppError, Ctx};
use interfaces::{BHandler, BRoutes, BSender};
use messages::*;

pub struct B {
    trace: Arc<Mutex<Vec<String>>>,
}

impl B {
    pub fn new(trace: Arc<Mutex<Vec<String>>>) -> Self {
        B { trace }
    }
}

impl<R: BRoutes> BHandler<R> for B {
    async fn handle_pong(
        &self,
        ctx: &Ctx,
        msg: Pong,
        s: BSender<'_, R>,
    ) -> Result<Count, AppError> {
        let below = s.send_ping(ctx, Ping { depth: msg.depth }).await?;
        Ok(Count {
            hops: below.hops + 1,
        })
    }

    async fn handle_event(
        &self,
        _ctx: &Ctx,
        msg: Event,
        _s: BSender<'_, R>,
    ) -> Result<(), AppError> {
        if msg.fail_at == Some("b") {
            return Err(AppError::internal("b failed"));
        }
        self.trace.lock().unwrap().push("b".to_string());
        Ok(())
    }
}

/// No loops: the default.
impl<R> Component<R> for B {}
