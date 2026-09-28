//! The wake bus: ONE `LISTEN` connection per process on the
//! processing-object wake channel, fanning each notification to the
//! worker of the type it names. A worker registered with the app takes
//! its wakes from here instead of pinning a listen connection of its own
//! (the library default for a standalone worker), so a replica with N
//! types holds one listener, not N. The poll stays the correctness path:
//! a notification lost across a reconnect costs latency, never work.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use basable_core::Ctx;
use basable_processingobject::{WAKE_CHANNEL, WakeSubscription};
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::watch;

/// Paces reconnecting after the listen connection fails.
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// The per-process wake listener.
pub struct WakeBus {
    pool: PgPool,
    subscriptions: Mutex<BTreeMap<&'static str, Vec<WakeSubscription>>>,
    listening: watch::Sender<bool>,
}

impl WakeBus {
    /// A bus over the framework's own pool (the `app` login).
    pub fn new(pool: PgPool) -> WakeBus {
        WakeBus {
            pool,
            subscriptions: Mutex::new(BTreeMap::new()),
            listening: watch::Sender::new(false),
        }
    }

    /// A subscription woken by every notification naming `type_name`. A
    /// subscription made after [`WakeBus::run`] started is served too: the
    /// map is read per notification.
    pub fn subscribe(&self, type_name: &'static str) -> WakeSubscription {
        let sub = WakeSubscription::new();
        self.subscriptions
            .lock()
            .expect("subscriptions lock")
            .entry(type_name)
            .or_default()
            .push(sub.clone());
        sub
    }

    /// Whether the listen connection is up right now.
    pub fn is_listening(&self) -> bool {
        *self.listening.borrow()
    }

    /// Resolves once the listen connection is up.
    pub async fn listening(&self) {
        let mut rx = self.listening.subscribe();
        let _ = rx.wait_for(|up| *up).await;
    }

    /// Listens until `ctx` is cancelled, reconnecting after a fixed delay
    /// on a connection error. A lost-and-restored connection wakes every
    /// subscription once: the gap may have hidden a wake, and a scan is
    /// cheap.
    pub async fn run(&self, ctx: Ctx) {
        while !ctx.is_cancelled() {
            let outcome = self.listen(&ctx).await;
            self.listening.send_replace(false);
            match outcome {
                Ok(()) => return,
                Err(e) => {
                    if ctx.is_cancelled() {
                        return;
                    }
                    tracing::warn!(error = %e, "wake bus listener reconnecting");
                }
            }
            tokio::select! {
                _ = ctx.cancelled() => return,
                _ = tokio::time::sleep(RECONNECT_DELAY) => {}
            }
        }
    }

    async fn listen(&self, ctx: &Ctx) -> Result<(), sqlx::Error> {
        let mut listener = PgListener::connect_with(&self.pool).await?;
        listener.listen(WAKE_CHANNEL).await?;
        self.listening.send_replace(true);
        tracing::info!(channel = WAKE_CHANNEL, "wake bus listening");
        loop {
            let notification = tokio::select! {
                n = listener.try_recv() => n?,
                _ = ctx.cancelled() => return Ok(()),
            };
            match notification {
                Some(n) => self.wake(n.payload()),
                None => {
                    tracing::warn!(
                        "wake bus connection was lost and re-established; waking every worker"
                    );
                    self.wake_all();
                }
            }
        }
    }

    fn wake(&self, type_name: &str) {
        let subs = self.subscriptions.lock().expect("subscriptions lock");
        if let Some(list) = subs.get(type_name) {
            for s in list {
                s.wake();
            }
        }
    }

    fn wake_all(&self) {
        let subs = self.subscriptions.lock().expect("subscriptions lock");
        for s in subs.values().flatten() {
            s.wake();
        }
    }
}

impl std::fmt::Debug for WakeBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let subs = self.subscriptions.lock().expect("subscriptions lock");
        f.debug_struct("WakeBus")
            .field("types", &subs.keys().collect::<Vec<_>>())
            .field("listening", &self.is_listening())
            .finish()
    }
}
