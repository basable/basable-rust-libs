//! Plain tickers: the schedules a nanoservice owns, the shape of the
//! monorepo's `gitoperator/worker.go`. An immediate first tick (a long
//! interval must not wait out a restart), then one per interval; a
//! failing tick is logged and retried at the next; joined on shutdown.
//! Never a CronJob.

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use basable_core::{BoxError, Ctx};

/// A tick's future: it borrows the tick's context.
pub type TickFuture<'a> = Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + 'a>>;

type TickFn = Box<dyn for<'a> Fn(&'a Ctx) -> TickFuture<'a> + Send + Sync>;

/// One schedule: a name, an interval and the tick.
pub struct Ticker {
    name: String,
    every: Duration,
    tick: TickFn,
}

impl Ticker {
    /// A ticker. `tick` runs at start and then every `every`; its context
    /// is a child of the app's, cancelled on shutdown.
    pub fn new<F>(name: impl Into<String>, every: Duration, tick: F) -> Ticker
    where
        F: for<'a> Fn(&'a Ctx) -> TickFuture<'a> + Send + Sync + 'static,
    {
        Ticker {
            name: name.into(),
            every,
            tick: Box::new(tick),
        }
    }

    /// The name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The interval.
    pub fn every(&self) -> Duration {
        self.every
    }

    /// Ticks until `ctx` is cancelled. A tick that fails or panics is
    /// logged; the next tick runs as scheduled.
    pub async fn run(self, ctx: Ctx) {
        let name = self.name.as_str();
        tracing::info!(
            ticker = name,
            every_ms = self.every.as_millis() as u64,
            "ticker starting"
        );
        loop {
            let tick_ctx = ctx.child();
            match catch_unwind_async((self.tick)(&tick_ctx)).await {
                Ok(Ok(())) => tracing::debug!(ticker = name, "tick done"),
                Ok(Err(e)) => {
                    tracing::error!(ticker = name, error = %e, "tick failed; retrying at the next interval")
                }
                Err(panic) => tracing::error!(
                    ticker = name,
                    panic,
                    "tick panicked; retrying at the next interval"
                ),
            }
            tokio::select! {
                _ = ctx.cancelled() => break,
                _ = tokio::time::sleep(self.every) => {}
            }
        }
        tracing::info!(ticker = name, "ticker stopped");
    }
}

impl std::fmt::Debug for Ticker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticker")
            .field("name", &self.name)
            .field("every", &self.every)
            .finish_non_exhaustive()
    }
}

/// Polls `fut` with each poll under `catch_unwind`; a panic becomes `Err`
/// with its message.
async fn catch_unwind_async<F: Future>(fut: F) -> Result<F::Output, String> {
    let mut fut = Box::pin(fut);
    std::future::poll_fn(
        |cx| match catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(out)) => Poll::Ready(Ok(out)),
            Err(payload) => Poll::Ready(Err(panic_message(&*payload))),
        },
    )
    .await
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[tokio::test]
    async fn ticks_at_once_then_per_interval_and_survives_failures_and_panics() {
        let ticks = Arc::new(AtomicU32::new(0));
        let t = ticks.clone();
        let ticker = Ticker::new("t", Duration::from_millis(20), move |_ctx| {
            let t = t.clone();
            Box::pin(async move {
                let n = t.fetch_add(1, Ordering::SeqCst);
                match n % 3 {
                    1 => Err("no".into()),
                    2 => panic!("boom"),
                    _ => Ok(()),
                }
            })
        });
        let ctx = Ctx::background();
        let task = tokio::spawn(ticker.run(ctx.clone()));
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(
            ticks.load(Ordering::SeqCst),
            1,
            "the first tick is immediate"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            ticks.load(Ordering::SeqCst) >= 4,
            "kept ticking through a failure and a panic"
        );
        ctx.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("the ticker stops on cancellation")
            .unwrap();
    }

    #[tokio::test]
    async fn the_tick_context_is_cancelled_with_the_ticker() {
        let ticker = Ticker::new("slow", Duration::from_secs(3600), |ctx| {
            Box::pin(async move {
                ctx.cancelled().await;
                Ok(())
            })
        });
        let ctx = Ctx::background();
        let task = tokio::spawn(ticker.run(ctx.clone()));
        tokio::time::sleep(Duration::from_millis(10)).await;
        ctx.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("a tick waiting on its context ends with the ticker")
            .unwrap();
    }
}
