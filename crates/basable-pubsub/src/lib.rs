//! Cross-replica publish/subscribe over Postgres `LISTEN`/`NOTIFY`, the port
//! of the monorepo's `golang/lib/pubsub`. A nanoservice project runs as
//! several identical replicas; the [`Bus`] lets one replica broadcast a
//! message that handlers on every replica receive, with Postgres as the
//! transport and no broker.
//!
//! One `Bus` per process multiplexes every logical channel over one listen
//! connection and runs as a background task ([`Bus::run`]). Every
//! [`Bus::subscribe`] and [`Bus::on_reconnect`] happens BEFORE `run`: the
//! set of channels is fixed once the connection listens. Own-message dedup
//! is built in: every payload carries the publishing bus's instance id and
//! a bus skips its own messages unless [`Bus::deliver_to_self`] was set.
//!
//! This is the broadcast channel (SSE fan-out, cross-replica cancels). The
//! processing-object wake channel is separate and owned by
//! `basable-processingobject` and the app's `WakeBus`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use basable_core::Ctx;
use sqlx::postgres::PgListener;
use sqlx::{PgConnection, PgPool};
use tokio::sync::watch;
use uuid::Uuid;

/// Postgres's hard limit on a `NOTIFY` payload.
pub const NOTIFY_MAX_BYTES: usize = 8000;

/// The fixed length of the canonical UUID origin prefix on every wire
/// payload.
const ORIGIN_LEN: usize = 36;

/// The largest application payload [`Bus::publish`] accepts: the `NOTIFY`
/// limit less the origin prefix and a safety margin. A caller with a bigger
/// payload truncates to this budget first.
pub const MAX_DATA_BYTES: usize = NOTIFY_MAX_BYTES - ORIGIN_LEN - 64;

/// Paces reconnecting after the listen connection fails.
const RECONNECT_DELAY: Duration = Duration::from_secs(3);

/// Whether a notification published inside a transaction reaches the
/// publishing bus as well as its siblings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Siblings only: the publisher already acted locally.
    ExcludeSelf,
    /// Every replica including the publisher, whatever
    /// [`Bus::deliver_to_self`] says: the origin is the nil UUID.
    IncludeSelf,
}

/// A received notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The channel.
    pub channel: String,
    /// The instance id of the publishing bus (the nil UUID for an
    /// `IncludeSelf` publish).
    pub origin: String,
    /// The application payload exactly as published.
    pub data: String,
}

/// Handles a received [`Message`]. It runs on the bus's listen task, so it
/// must not block; slow work goes to its own task.
pub type Handler = Box<dyn Fn(Message) + Send + Sync>;

/// A hook run after the bus re-establishes its listen connection:
/// notifications may have been missed during the gap, so a subscriber that
/// needs gap recovery (a resync broadcast, say) registers here.
pub type ReconnectHook = Box<dyn Fn() + Send + Sync>;

/// Why a subscription was refused.
#[derive(Debug)]
pub enum SubscribeError {
    /// [`Bus::run`] has started; the channel set is fixed.
    AfterRun,
    /// The channel name is empty or contains a NUL.
    InvalidChannel(String),
}

impl fmt::Display for SubscribeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubscribeError::AfterRun => f.write_str("pubsub: subscribe after run"),
            SubscribeError::InvalidChannel(c) => write!(f, "pubsub: invalid channel name {c:?}"),
        }
    }
}

impl std::error::Error for SubscribeError {}

/// Why a publish failed.
#[derive(Debug)]
pub enum PublishError {
    /// The payload exceeds [`MAX_DATA_BYTES`].
    PayloadTooLarge {
        /// The payload's length in bytes.
        len: usize,
    },
    /// The channel name is empty or contains a NUL.
    InvalidChannel(String),
    /// The `pg_notify` failed.
    Database(sqlx::Error),
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PublishError::PayloadTooLarge { len } => {
                write!(f, "pubsub: payload of {len} bytes exceeds {MAX_DATA_BYTES}")
            }
            PublishError::InvalidChannel(c) => write!(f, "pubsub: invalid channel name {c:?}"),
            PublishError::Database(e) => write!(f, "pubsub publish: {e}"),
        }
    }
}

impl std::error::Error for PublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PublishError::Database(e) => Some(e),
            _ => None,
        }
    }
}

/// A cross-replica pub/sub bus over Postgres `LISTEN`/`NOTIFY`.
pub struct Bus {
    pool: PgPool,
    instance_id: String,
    deliver_self: bool,
    handlers: Mutex<BTreeMap<String, Vec<Handler>>>,
    on_reconnect: Mutex<Vec<ReconnectHook>>,
    started: AtomicBool,
    listening: watch::Sender<bool>,
}

impl fmt::Debug for Bus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bus")
            .field("instance_id", &self.instance_id)
            .field("deliver_self", &self.deliver_self)
            .finish_non_exhaustive()
    }
}

impl Bus {
    /// A bus over `pool`: the app login, which may `NOTIFY` and `LISTEN`.
    /// The listen connection is taken from this pool while [`Bus::run`]
    /// runs and returned with its subscriptions cleared.
    pub fn new(pool: PgPool) -> Bus {
        Bus {
            pool,
            instance_id: Uuid::new_v4().to_string(),
            deliver_self: false,
            handlers: Mutex::new(BTreeMap::new()),
            on_reconnect: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
            listening: watch::Sender::new(false),
        }
    }

    /// Whether the listen connection is up right now. False before
    /// [`Bus::run`], during a reconnect, and after the run ends; a publish
    /// made while false reaches the siblings that are listening but not
    /// this bus.
    pub fn is_listening(&self) -> bool {
        *self.listening.borrow()
    }

    /// Resolves once the listen connection is up (or at once if it is).
    pub async fn listening(&self) {
        let mut rx = self.listening.subscribe();
        let _ = rx.wait_for(|up| *up).await;
    }

    /// Delivers a publisher's own messages back to its local handlers too.
    /// Off by default: the publisher usually acted locally already and only
    /// needs its siblings.
    pub fn deliver_to_self(mut self) -> Bus {
        self.deliver_self = true;
        self
    }

    /// This bus's per-process identifier, stamped on every published
    /// message.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Registers a handler for `channel`. Before [`Bus::run`] only.
    pub fn subscribe(
        &self,
        channel: &str,
        handler: impl Fn(Message) + Send + Sync + 'static,
    ) -> Result<(), SubscribeError> {
        if !valid_channel(channel) {
            return Err(SubscribeError::InvalidChannel(channel.to_string()));
        }
        if self.started.load(Ordering::SeqCst) {
            return Err(SubscribeError::AfterRun);
        }
        self.handlers
            .lock()
            .expect("handlers lock")
            .entry(channel.to_string())
            .or_default()
            .push(Box::new(handler));
        Ok(())
    }

    /// Registers a reconnect hook. Before [`Bus::run`] only.
    pub fn on_reconnect(
        &self,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> Result<(), SubscribeError> {
        if self.started.load(Ordering::SeqCst) {
            return Err(SubscribeError::AfterRun);
        }
        self.on_reconnect
            .lock()
            .expect("hooks lock")
            .push(Box::new(hook));
        Ok(())
    }

    /// Broadcasts `data` on `channel` to the handlers on every replica
    /// (this one included only with [`Bus::deliver_to_self`]).
    pub async fn publish(&self, channel: &str, data: &str) -> Result<(), PublishError> {
        let wire = self.wire_payload(channel, data, false)?;
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(channel)
            .bind(wire)
            .execute(&self.pool)
            .await
            .map_err(PublishError::Database)?;
        Ok(())
    }

    /// Queues the notification in the caller's transaction: Postgres
    /// delivers it only if that transaction commits. `IncludeSelf` uses the
    /// nil origin so the publishing bus receives it even with own-message
    /// dedup on.
    pub async fn publish_tx(
        &self,
        tx: &mut PgConnection,
        channel: &str,
        data: &str,
        delivery: Delivery,
    ) -> Result<(), PublishError> {
        let wire = self.wire_payload(channel, data, delivery == Delivery::IncludeSelf)?;
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(channel)
            .bind(wire)
            .execute(tx)
            .await
            .map_err(PublishError::Database)?;
        Ok(())
    }

    fn wire_payload(
        &self,
        channel: &str,
        data: &str,
        include_self: bool,
    ) -> Result<String, PublishError> {
        if !valid_channel(channel) {
            return Err(PublishError::InvalidChannel(channel.to_string()));
        }
        if data.len() > MAX_DATA_BYTES {
            return Err(PublishError::PayloadTooLarge { len: data.len() });
        }
        let origin = if include_self {
            Uuid::nil().to_string()
        } else {
            self.instance_id.clone()
        };
        let mut wire = String::with_capacity(ORIGIN_LEN + data.len());
        wire.push_str(&origin);
        wire.push_str(data);
        Ok(wire)
    }

    /// Listens on every subscribed channel and dispatches notifications
    /// until `ctx` is cancelled, reconnecting after a fixed delay on a
    /// connection error and firing the reconnect hooks after each
    /// re-establishment. With no subscriptions it idles until cancelled.
    pub async fn run(&self, ctx: Ctx) {
        self.started.store(true, Ordering::SeqCst);
        let channels: Vec<String> = self
            .handlers
            .lock()
            .expect("handlers lock")
            .keys()
            .cloned()
            .collect();
        if channels.is_empty() {
            ctx.cancelled().await;
            return;
        }
        let mut reconnect = false;
        while !ctx.is_cancelled() {
            let outcome = self.listen(&ctx, &channels, reconnect).await;
            self.listening.send_replace(false);
            match outcome {
                Ok(()) => return,
                Err(e) => {
                    if ctx.is_cancelled() {
                        return;
                    }
                    tracing::error!(error = %e, delay_ms = RECONNECT_DELAY.as_millis() as u64, "pubsub: listen loop error, reconnecting");
                }
            }
            reconnect = true;
            tokio::select! {
                _ = ctx.cancelled() => return,
                _ = tokio::time::sleep(RECONNECT_DELAY) => {}
            }
        }
    }

    /// One listen connection's life: `Ok(())` when the context ended,
    /// `Err` when the connection failed and could not be re-established
    /// by the listener itself.
    async fn listen(
        &self,
        ctx: &Ctx,
        channels: &[String],
        reconnect: bool,
    ) -> Result<(), sqlx::Error> {
        // The listener returns its connection to the pool with `UNLISTEN *`
        // on drop, so a reconnect leaves no subscribed connection behind
        // (the port of Go's db.ReleaseListenConn, done by sqlx).
        let mut listener = PgListener::connect_with(&self.pool).await?;
        let names: Vec<&str> = channels.iter().map(String::as_str).collect();
        listener.listen_all(names).await?;
        self.listening.send_replace(true);
        if reconnect {
            self.fire_reconnect();
        }
        tracing::info!(channels = ?channels, "pubsub: listening");
        loop {
            let notification = tokio::select! {
                n = listener.try_recv() => n?,
                _ = ctx.cancelled() => return Ok(()),
            };
            match notification {
                Some(n) => self.dispatch(n.channel(), n.payload()),
                // A lost-and-restored connection: the listener re-issued its
                // LISTENs; the gap is the hooks' to recover.
                None => {
                    tracing::warn!("pubsub: listen connection was lost and re-established");
                    self.fire_reconnect();
                }
            }
        }
    }

    fn dispatch(&self, channel: &str, wire: &str) {
        if wire.len() < ORIGIN_LEN || !wire.is_char_boundary(ORIGIN_LEN) {
            tracing::warn!(
                channel,
                "pubsub: malformed payload (shorter than the origin prefix)"
            );
            return;
        }
        let (origin, data) = wire.split_at(ORIGIN_LEN);
        if origin == self.instance_id && !self.deliver_self {
            return;
        }
        let handlers = self.handlers.lock().expect("handlers lock");
        let Some(hs) = handlers.get(channel) else {
            return;
        };
        let msg = Message {
            channel: channel.to_string(),
            origin: origin.to_string(),
            data: data.to_string(),
        };
        for h in hs {
            h(msg.clone());
        }
    }

    fn fire_reconnect(&self) {
        for hook in self.on_reconnect.lock().expect("hooks lock").iter() {
            hook();
        }
    }
}

/// `LISTEN` cannot be parameterised; the listener quotes the channel as an
/// identifier, so only an empty name or a NUL is refused here.
fn valid_channel(channel: &str) -> bool {
    !channel.is_empty() && !channel.contains('\0')
}

/// A shared bus: what the app hands to every component that publishes.
pub type SharedBus = Arc<Bus>;

#[cfg(test)]
mod tests {
    use super::*;

    fn bus() -> Bus {
        Bus::new(PgPool::connect_lazy("postgres://x@localhost/x").expect("lazy pool"))
    }

    #[tokio::test]
    async fn the_wire_payload_carries_the_origin_and_bounds_the_data() {
        let b = bus();
        let wire = b.wire_payload("c", "hello", false).unwrap();
        assert_eq!(&wire[..ORIGIN_LEN], b.instance_id());
        assert_eq!(&wire[ORIGIN_LEN..], "hello");
        let wire = b.wire_payload("c", "hello", true).unwrap();
        assert_eq!(&wire[..ORIGIN_LEN], "00000000-0000-0000-0000-000000000000");
        let big = "x".repeat(MAX_DATA_BYTES + 1);
        assert!(matches!(
            b.wire_payload("c", &big, false),
            Err(PublishError::PayloadTooLarge { len }) if len == MAX_DATA_BYTES + 1
        ));
        assert!(
            b.wire_payload("c", &"x".repeat(MAX_DATA_BYTES), false)
                .is_ok()
        );
        assert!(matches!(
            b.wire_payload("", "x", false),
            Err(PublishError::InvalidChannel(_))
        ));
    }

    #[tokio::test]
    async fn dispatch_skips_own_messages_unless_asked_and_tolerates_junk() {
        let b = bus();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        b.subscribe("c", move |m| s.lock().unwrap().push(m))
            .unwrap();
        let own = b.wire_payload("c", "mine", false).unwrap();
        b.dispatch("c", &own);
        assert!(seen.lock().unwrap().is_empty());
        let other = format!("{}{}", Uuid::new_v4(), "theirs");
        b.dispatch("c", &other);
        b.dispatch("c", "short");
        b.dispatch("other", &other);
        let got = seen.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, "theirs");
        assert_eq!(got[0].channel, "c");

        let b = bus().deliver_to_self();
        let s = seen.clone();
        b.subscribe("c", move |m| s.lock().unwrap().push(m))
            .unwrap();
        let own = b.wire_payload("c", "mine", false).unwrap();
        b.dispatch("c", &own);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn subscribing_after_run_or_on_a_bad_channel_is_refused() {
        let b = bus();
        assert!(matches!(
            b.subscribe("", |_| {}),
            Err(SubscribeError::InvalidChannel(_))
        ));
        assert!(matches!(
            b.subscribe("a\0b", |_| {}),
            Err(SubscribeError::InvalidChannel(_))
        ));
        b.started.store(true, Ordering::SeqCst);
        assert!(matches!(
            b.subscribe("c", |_| {}),
            Err(SubscribeError::AfterRun)
        ));
        assert!(matches!(
            b.on_reconnect(|| {}),
            Err(SubscribeError::AfterRun)
        ));
    }
}
