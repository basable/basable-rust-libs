//! `a`: answers a Ping by asking `b` (a Pong) until the depth is spent.

use std::sync::{Arc, Mutex};

use basable_app::Component;
use basable_core::{AppError, Ctx};
use interfaces::{AHandler, ARoutes, ASender};
use messages::*;

pub struct A {
    trace: Arc<Mutex<Vec<String>>>,
}

impl A {
    pub fn new(trace: Arc<Mutex<Vec<String>>>) -> Self {
        A { trace }
    }
}

impl<R: ARoutes> AHandler<R> for A {
    async fn handle_ping(
        &self,
        ctx: &Ctx,
        msg: Ping,
        s: ASender<'_, R>,
    ) -> Result<Count, AppError> {
        if msg.depth == 0 {
            return Ok(Count { hops: 1 });
        }
        let below = s
            .send_pong(
                ctx,
                Pong {
                    depth: msg.depth - 1,
                },
            )
            .await?;
        Ok(Count {
            hops: below.hops + 1,
        })
    }

    async fn handle_event(
        &self,
        _ctx: &Ctx,
        msg: Event,
        _s: ASender<'_, R>,
    ) -> Result<(), AppError> {
        if msg.fail_at == Some("a") {
            return Err(AppError::internal("a failed"));
        }
        self.trace.lock().unwrap().push("a".to_string());
        Ok(())
    }
}

/// No loops: the default.
impl<R> Component<R> for A {}
