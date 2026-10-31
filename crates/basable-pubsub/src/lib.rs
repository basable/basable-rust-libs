//! Cross-replica publish/subscribe, the port of the monorepo's
//! `golang/lib/pubsub` (both rebuilt 2026-10-31; until then they were
//! `LISTEN`/`NOTIFY`).
//!
//! A subscription is a row in Postgres naming the channel and the replica
//! that subscribed; a publish reads the channel's subscribers there and sends
//! the message over a connection straight to each of them. Postgres is the
//! directory, never the transport: no payload passes through it, so a message
//! may be up to [`MAX_MESSAGE_BYTES`], and only the replicas that subscribed
//! to a channel receive what is published on it.
//!
//! Each process is one replica. [`Bus::new`] registers it: an id made at
//! start, the address its peers dial (the advertised host and the port its
//! listener got), and the fingerprint of the certificate it made at start.
//! [`Bus::run`] heartbeats it, deletes any replica whose heartbeat is older
//! than the expiry (its subscriptions with it), and deregisters it when it
//! returns. Peers talk mutual TLS 1.3, each side accepting only the
//! certificate the directory lists for the replica at the other end: the
//! database is the trust anchor.
//!
//! Delivery is at most once. A publish reaches every subscriber, this
//! replica's own included (in process), and returns how many replicas took
//! the message. Messages from one publisher to one replica arrive in the
//! order they were published. A replica that finds it was swept registers
//! again and runs its [`Bus::on_reregister`] hooks, where a subscriber that
//! needs gap recovery resyncs.
//!
//! The directory is the framework's tables `basable.pubsub_replica` and
//! `basable.pubsub_subscription` (the scaffold's framework migration
//! `20260101000003_basable_pubsub_directory.sql`), read and written as the
//! `app` login.
//!
//! This is the channel between replicas (SSE fan-out, cross-replica
//! cancels). A processing-object wake never travels over it: that wake is in
//! process, from a store to the workers running on it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod directory;
mod peer;

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use basable_core::Ctx;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use uuid::Uuid;

/// How often a running replica heartbeats its row and sweeps the directory.
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);
/// How old a replica's heartbeat may be before the sweep deletes it, and
/// before a publish stops counting it as a subscriber.
pub const DEFAULT_EXPIRY: Duration = Duration::from_secs(120);
/// Bounds one delivery to one replica: the dial, the write and the
/// acknowledgement.
pub const DEFAULT_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest message [`Bus::publish`] accepts.
pub const MAX_MESSAGE_BYTES: usize = 16 << 20;
/// The longest channel name.
pub const MAX_CHANNEL_BYTES: usize = 512;

/// A replica's place on the network and its timings.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the replica accepts its peers, `host:port`; port 0 lets the
    /// kernel pick one, which the directory then publishes.
    pub listen_address: String,
    /// The host its peers dial, the pod IP. `None` takes the listener's own
    /// host, or 127.0.0.1 when that is unspecified.
    pub advertise_host: Option<String>,
    /// [`DEFAULT_HEARTBEAT_INTERVAL`] by default.
    pub heartbeat_interval: Duration,
    /// [`DEFAULT_EXPIRY`] by default; must exceed the heartbeat interval.
    pub expiry: Duration,
    /// [`DEFAULT_SEND_TIMEOUT`] by default.
    pub send_timeout: Duration,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            listen_address: "0.0.0.0:0".to_string(),
            advertise_host: None,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            expiry: DEFAULT_EXPIRY,
            send_timeout: DEFAULT_SEND_TIMEOUT,
        }
    }
}

/// One delivered message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The channel.
    pub channel: String,
    /// The id of the replica that published it.
    pub origin: String,
    /// The message as published.
    pub data: Vec<u8>,
}

/// Handles a delivered [`Message`]. It runs on the task that delivers it
/// (the publisher's own for an in-process delivery, the peer connection's
/// reader otherwise), so it must not block: messages from one peer wait
/// behind it. Slow work goes to its own task.
pub type Handler = Arc<dyn Fn(&Message) + Send + Sync>;

/// A hook run after the replica registers again, having found itself swept
/// from the directory: messages published to it in the meantime were lost.
pub type ReregisterHook = Arc<dyn Fn() + Send + Sync>;

/// Why a bus call failed.
#[derive(Debug)]
pub enum Error {
    /// The channel name is empty, longer than [`MAX_CHANNEL_BYTES`], or
    /// holds a NUL.
    InvalidChannel(String),
    /// The message exceeds [`MAX_MESSAGE_BYTES`].
    MessageTooLarge {
        /// The message's length in bytes.
        len: usize,
    },
    /// The configuration cannot work.
    Config(String),
    /// The directory could not be read or written.
    Database(sqlx::Error),
    /// The listener could not be opened.
    Io(std::io::Error),
    /// The replica's certificate or TLS configuration could not be made.
    Tls(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidChannel(c) => write!(f, "pubsub: invalid channel name {c:?}"),
            Error::MessageTooLarge { len } => {
                write!(
                    f,
                    "pubsub: message of {len} bytes exceeds {MAX_MESSAGE_BYTES}"
                )
            }
            Error::Config(e) => write!(f, "pubsub: {e}"),
            Error::Database(e) => write!(f, "pubsub: directory: {e}"),
            Error::Io(e) => write!(f, "pubsub: listen: {e}"),
            Error::Tls(e) => write!(f, "pubsub: tls: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Database(e) => Some(e),
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Error {
        Error::Database(e)
    }
}

/// One replica's membership: its subscriptions, its connections to its peers
/// and its row in the directory. Cheap to clone; every clone is the same
/// replica.
#[derive(Clone)]
pub struct Bus {
    inner: Arc<Inner>,
}

impl fmt::Debug for Bus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bus")
            .field("replica", &self.inner.id)
            .field("address", &self.inner.address)
            .finish_non_exhaustive()
    }
}

struct Inner {
    /// The database holding the directory; `None` for a local bus.
    pool: Option<PgPool>,
    cfg: Config,
    id: Uuid,
    address: String,
    /// Taken by [`Bus::run`].
    listener: Mutex<Option<TcpListener>>,
    /// The certificate and TLS configuration; `None` for a local bus.
    identity: Option<peer::Identity>,
    /// Serialises the directory writes that mirror `subs` (the first handler
    /// on a channel inserts its row, the last one out deletes it, a
    /// registration inserts every channel), so the rows and the map agree.
    dir: tokio::sync::Mutex<()>,
    subs: RwLock<BTreeMap<String, Vec<(u64, Handler)>>>,
    next_subscription: AtomicU64,
    hooks: Mutex<Vec<ReregisterHook>>,
    /// This replica's connections to the others, dialled on first publish.
    peers: tokio::sync::Mutex<HashMap<Uuid, Arc<peer::Peer>>>,
}

impl Bus {
    /// Registers this process as a replica in the directory and opens its
    /// listener. Subscriptions may be made at once; peers are served, and the
    /// replica heartbeated, once [`Bus::run`] runs.
    pub async fn new(pool: PgPool, cfg: Config) -> Result<Bus, Error> {
        if cfg.expiry <= cfg.heartbeat_interval {
            return Err(Error::Config(format!(
                "expiry {:?} must exceed the heartbeat interval {:?}",
                cfg.expiry, cfg.heartbeat_interval
            )));
        }
        let id = Uuid::new_v4();
        let identity = peer::Identity::new(id)?;
        let listener = TcpListener::bind(&cfg.listen_address)
            .await
            .map_err(Error::Io)?;
        let address = advertised_address(
            listener.local_addr().map_err(Error::Io)?,
            cfg.advertise_host.as_deref(),
        );
        let bus = Bus::with(Some(pool), cfg, id, address, Some(listener), Some(identity));
        {
            let _dir = bus.inner.dir.lock().await;
            directory::register(&bus.inner).await?;
        }
        tracing::info!(replica = %id, address = %bus.inner.address, "pubsub: registered");
        Ok(bus)
    }

    /// A bus of one process with no directory and no peers: a publish
    /// reaches this process's own subscribers only. For a process that is
    /// the only replica by construction, and for tests that run without a
    /// database.
    pub fn local() -> Bus {
        Bus::with(
            None,
            Config::default(),
            Uuid::new_v4(),
            String::new(),
            None,
            None,
        )
    }

    fn with(
        pool: Option<PgPool>,
        cfg: Config,
        id: Uuid,
        address: String,
        listener: Option<TcpListener>,
        identity: Option<peer::Identity>,
    ) -> Bus {
        Bus {
            inner: Arc::new(Inner {
                pool,
                cfg,
                id,
                address,
                listener: Mutex::new(listener),
                identity,
                dir: tokio::sync::Mutex::new(()),
                subs: RwLock::new(BTreeMap::new()),
                next_subscription: AtomicU64::new(1),
                hooks: Mutex::new(Vec::new()),
                peers: tokio::sync::Mutex::new(HashMap::new()),
            }),
        }
    }

    /// This replica's id: the origin of every message it publishes.
    pub fn replica_id(&self) -> String {
        self.inner.id.to_string()
    }

    /// The address this replica's peers dial, as the directory lists it;
    /// empty for a local bus.
    pub fn address(&self) -> &str {
        &self.inner.address
    }

    /// Registers a hook run after this replica registers again, having found
    /// itself swept from the directory. Never run for the first registration.
    pub fn on_reregister(&self, hook: impl Fn() + Send + Sync + 'static) {
        self.inner
            .hooks
            .lock()
            .expect("hooks lock")
            .push(Arc::new(hook));
    }

    /// Adds a handler to `channel`. The replica's row for the channel is
    /// written with the first handler on it, so publishes from every replica
    /// reach it from the moment this returns.
    pub async fn subscribe(
        &self,
        channel: &str,
        handler: impl Fn(&Message) + Send + Sync + 'static,
    ) -> Result<Subscription, Error> {
        valid_channel(channel)?;
        let dir = self.inner.dir.lock().await;
        let first = self
            .inner
            .subs
            .read()
            .expect("subs lock")
            .get(channel)
            .is_none_or(Vec::is_empty);
        let reregistered = if first && self.inner.pool.is_some() {
            directory::insert_subscription(&self.inner, channel).await?
        } else {
            false
        };
        let id = self.inner.next_subscription.fetch_add(1, Ordering::Relaxed);
        self.inner
            .subs
            .write()
            .expect("subs lock")
            .entry(channel.to_string())
            .or_default()
            .push((id, Arc::new(handler)));
        drop(dir);
        if reregistered {
            self.inner.run_reregister_hooks();
        }
        Ok(Subscription {
            bus: self.clone(),
            channel: channel.to_string(),
            id,
        })
    }

    /// Sends `data` to every subscriber of `channel` and returns how many
    /// replicas took it, this one included when it subscribes. A replica
    /// that cannot be reached, or no longer subscribes, is not counted and
    /// is logged. This replica's own subscribers are served before the
    /// directory is read, so they get the message even when this fails.
    pub async fn publish(&self, channel: &str, data: &[u8]) -> Result<usize, Error> {
        valid_channel(channel)?;
        if data.len() > MAX_MESSAGE_BYTES {
            return Err(Error::MessageTooLarge { len: data.len() });
        }
        let mut delivered = 0;
        let local = Message {
            channel: channel.to_string(),
            origin: self.replica_id(),
            data: data.to_vec(),
        };
        if self.inner.deliver_local(&local) {
            delivered += 1;
        }
        if self.inner.pool.is_none() {
            return Ok(delivered);
        }

        let targets = directory::subscribers(&self.inner, channel).await?;
        let data: Arc<[u8]> = Arc::from(data);
        let mut sends = JoinSet::new();
        for target in targets {
            let inner = self.inner.clone();
            let channel = channel.to_string();
            let data = data.clone();
            sends.spawn(async move {
                match peer::send(&inner, &target, &channel, &data).await {
                    Ok(taken) => taken,
                    Err(e) => {
                        tracing::warn!(channel, replica = %target.id, error = %e, "pubsub: delivery failed");
                        false
                    }
                }
            });
        }
        while let Some(taken) = sends.join_next().await {
            if taken.unwrap_or(false) {
                delivered += 1;
            }
        }
        Ok(delivered)
    }

    /// Serves this replica's peers and heartbeats it until `ctx` is
    /// cancelled, then closes every connection and deregisters it. A local
    /// bus only waits.
    pub async fn run(&self, ctx: Ctx) {
        let listener = self.inner.listener.lock().expect("listener lock").take();
        let Some(listener) = listener.filter(|_| self.inner.pool.is_some()) else {
            ctx.cancelled().await;
            return;
        };

        let mut tasks = JoinSet::new();
        {
            let inner = self.inner.clone();
            let ctx = ctx.clone();
            tasks.spawn(async move { peer::accept(inner, listener, ctx).await });
        }

        self.inner.tick().await;
        let mut ticker = tokio::time::interval(self.inner.cfg.heartbeat_interval);
        ticker.tick().await; // the first tick is immediate, and the tick above was it
        loop {
            tokio::select! {
                _ = ctx.cancelled() => break,
                _ = ticker.tick() => self.inner.tick().await,
            }
        }

        // Dropping this replica's connections closes them; the ones its
        // peers opened end with ctx, inside the accept task.
        self.inner.peers.lock().await.clear();
        while tasks.join_next().await.is_some() {}
        let send_timeout = self.inner.cfg.send_timeout;
        match tokio::time::timeout(send_timeout, directory::deregister(&self.inner)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(replica = %self.inner.id, error = %e, "pubsub: deregister failed; the sweep removes the row")
            }
            Err(_) => {
                tracing::warn!(replica = %self.inner.id, "pubsub: deregister timed out; the sweep removes the row")
            }
        }
    }
}

impl Inner {
    /// Hands `m` to this replica's handlers on its channel and reports
    /// whether there were any.
    fn deliver_local(&self, m: &Message) -> bool {
        let handlers: Vec<Handler> = self
            .subs
            .read()
            .expect("subs lock")
            .get(&m.channel)
            .map(|hs| hs.iter().map(|(_, h)| h.clone()).collect())
            .unwrap_or_default();
        for h in &handlers {
            h(m);
        }
        !handlers.is_empty()
    }

    fn run_reregister_hooks(&self) {
        let hooks: Vec<ReregisterHook> = self.hooks.lock().expect("hooks lock").clone();
        for hook in hooks {
            hook();
        }
    }

    /// Heartbeats this replica, registering it again when it was swept, and
    /// sweeps the replicas that stopped heartbeating.
    async fn tick(&self) {
        match directory::heartbeat(self).await {
            Ok(false) => {}
            Ok(true) => {
                tracing::warn!(replica = %self.id, "pubsub: this replica had been swept and registered again");
                self.run_reregister_hooks();
            }
            Err(e) => {
                tracing::error!(replica = %self.id, error = %e, "pubsub: heartbeat failed");
                return;
            }
        }
        match directory::sweep(self).await {
            Ok(0) => {}
            Ok(swept) => {
                tracing::info!(
                    count = swept,
                    "pubsub: swept replicas that stopped heartbeating"
                )
            }
            Err(e) => tracing::error!(error = %e, "pubsub: sweep failed"),
        }
    }
}

/// One handler on one channel, ended by [`Subscription::unsubscribe`].
/// Dropping it does not unsubscribe: that needs the directory.
#[derive(Debug)]
pub struct Subscription {
    bus: Bus,
    channel: String,
    id: u64,
}

impl Subscription {
    /// The channel.
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// Removes the handler. The replica's row for the channel goes with its
    /// last handler.
    pub async fn unsubscribe(self) -> Result<(), Error> {
        let inner = &self.bus.inner;
        let _dir = inner.dir.lock().await;
        let last = {
            let mut subs = inner.subs.write().expect("subs lock");
            let Some(handlers) = subs.get_mut(&self.channel) else {
                return Ok(());
            };
            let before = handlers.len();
            handlers.retain(|(id, _)| *id != self.id);
            if handlers.len() == before {
                return Ok(());
            }
            let empty = handlers.is_empty();
            if empty {
                subs.remove(&self.channel);
            }
            empty
        };
        if last && inner.pool.is_some() {
            directory::delete_subscription(inner, &self.channel).await?;
        }
        Ok(())
    }
}

fn valid_channel(channel: &str) -> Result<(), Error> {
    if channel.is_empty() || channel.len() > MAX_CHANNEL_BYTES || channel.contains('\0') {
        return Err(Error::InvalidChannel(channel.to_string()));
    }
    Ok(())
}

/// The `host:port` peers dial: the advertised host (or the listener's own)
/// with the port the listener got.
fn advertised_address(addr: SocketAddr, host: Option<&str>) -> String {
    let host = match host {
        Some(h) if !h.is_empty() => h.to_string(),
        _ if addr.ip().is_unspecified() => "127.0.0.1".to_string(),
        _ => addr.ip().to_string(),
    };
    if host.contains(':') {
        format!("[{host}]:{}", addr.port())
    } else {
        format!("{host}:{}", addr.port())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_local_bus_delivers_to_every_handler_and_counts_the_replica_once() {
        let bus = Bus::local();
        let seen = Arc::new(Mutex::new(Vec::new()));
        for tag in ["first", "second"] {
            let seen = seen.clone();
            bus.subscribe("orders", move |m| {
                seen.lock()
                    .unwrap()
                    .push(format!("{tag}:{}", String::from_utf8_lossy(&m.data)))
            })
            .await
            .unwrap();
        }
        assert_eq!(bus.publish("orders", b"one").await.unwrap(), 1);
        assert_eq!(*seen.lock().unwrap(), ["first:one", "second:one"]);
        assert_eq!(bus.publish("invoices", b"two").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_message_carries_the_replica_as_its_origin() {
        let bus = Bus::local();
        let got = Arc::new(Mutex::new(None));
        let g = got.clone();
        bus.subscribe("c", move |m| *g.lock().unwrap() = Some(m.clone()))
            .await
            .unwrap();
        bus.publish("c", b"x").await.unwrap();
        let m = got.lock().unwrap().clone().unwrap();
        assert_eq!(m.origin, bus.replica_id());
        assert_eq!(m.channel, "c");
    }

    #[tokio::test]
    async fn unsubscribing_stops_only_that_handler() {
        let bus = Bus::local();
        let kept = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let k = kept.clone();
        bus.subscribe("c", move |_| {
            k.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();
        let d = dropped.clone();
        let sub = bus
            .subscribe("c", move |_| {
                d.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .unwrap();
        sub.unsubscribe().await.unwrap();
        assert_eq!(bus.publish("c", b"").await.unwrap(), 1);
        assert_eq!(kept.load(Ordering::Relaxed), 1);
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn bad_channels_and_oversized_messages_are_refused() {
        let bus = Bus::local();
        let long = "x".repeat(MAX_CHANNEL_BYTES + 1);
        for channel in ["", long.as_str(), "a\0b"] {
            assert!(matches!(
                bus.publish(channel, b"").await,
                Err(Error::InvalidChannel(_))
            ));
            assert!(matches!(
                bus.subscribe(channel, |_| {}).await,
                Err(Error::InvalidChannel(_))
            ));
        }
        assert!(matches!(
            bus.publish("c", &vec![0; MAX_MESSAGE_BYTES + 1]).await,
            Err(Error::MessageTooLarge { len }) if len == MAX_MESSAGE_BYTES + 1
        ));
    }

    #[test]
    fn the_advertised_address_takes_the_advertised_host_and_the_listeners_port() {
        let addr: SocketAddr = "0.0.0.0:4123".parse().unwrap();
        assert_eq!(advertised_address(addr, Some("10.1.2.3")), "10.1.2.3:4123");
        assert_eq!(advertised_address(addr, None), "127.0.0.1:4123");
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        assert_eq!(advertised_address(addr, Some("")), "127.0.0.1:9");
        assert_eq!(advertised_address(addr, Some("fd00::1")), "[fd00::1]:9");
    }
}
