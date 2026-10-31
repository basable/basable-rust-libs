# basable-pubsub — publish/subscribe across replicas

The port of the monorepo's `golang/lib/pubsub` (both rebuilt 2026-10-31;
until then they were `LISTEN`/`NOTIFY`). A nanoservice project runs as
several identical replicas. A subscription is a row in Postgres naming the
channel and the replica; a publish reads the channel's subscribers there and
sends the message straight to each over a connection of their own. Postgres
is the directory, never the transport: messages are whole (up to 16 MiB),
and only the replicas that subscribed receive them. This is the channel
between replicas (SSE fan-out, cross-replica cancels). A processing-object
wake does not travel over it: it is in process, from a store to the workers
running on it (`docs/porting-notes.md` 81).

The Directive (`docs/DIRECTIVE.md` in every tenant repository,
`golang/controller/lib/scaffold/directive.md` in the monorepo) is the
contract this crate serves.

## The surface

| Item | What it is |
|---|---|
| `Bus::new(pool, Config).await` | Registers this process as a replica (a uuid made at start, the address its peers dial, its certificate's SHA-256) and opens its listener. `Config`: `listen_address` (`0.0.0.0:0` by default: the kernel picks the port, which the directory publishes), `advertise_host` (the pod IP; `None` takes the listener's host or 127.0.0.1), `heartbeat_interval` (60 s), `expiry` (120 s), `send_timeout` (5 s) |
| `Bus::local()` | A bus of one process, no directory, no peers: a publish reaches this process's own subscribers. For tests without a database |
| `subscribe(channel, handler).await` | Adds a `Handler` (`Arc<dyn Fn(&Message) + Send + Sync>`); the replica's row for the channel is written with the first handler on it. Any time, before or after `run`. Returns a `Subscription` |
| `Subscription::unsubscribe().await` | Removes that handler; the row goes with the channel's last one. Dropping a subscription does not unsubscribe |
| `publish(channel, data).await` | Sends to every subscriber, this replica's own in process first, and returns how many replicas took it; an unreachable one is logged and not counted |
| `on_reregister(hook)` | Runs after this replica registers again, having found itself swept (its heartbeat late past the expiry): what was published to it meanwhile was lost |
| `run(ctx).await` | Serves the peers, heartbeats this replica and sweeps the dead ones until `ctx` is cancelled, then closes its connections and deregisters it. A local bus only waits |
| `replica_id()`, `address()` | This replica's id (every message's `origin`) and the address the directory lists |
| `Message { channel, origin, data }` | A delivered message; `data` is the bytes as published |
| `Error` | `InvalidChannel`, `MessageTooLarge`, `Config`, `Database`, `Io`, `Tls` |
| `MAX_MESSAGE_BYTES` (16 MiB), `MAX_CHANNEL_BYTES` (512) | The limits |

## Rules

- **The directory is the framework's.** `basable.pubsub_replica` and
  `basable.pubsub_subscription`, the framework migration
  `20260101000003_basable_pubsub_directory.sql` (the scaffold renders it;
  a project scaffolded before it adds the file unchanged), read and written
  as the `app` login. No nanoservice touches them.
- **The database is the trust anchor.** Each replica makes a key pair and a
  self-signed certificate at start and lists the certificate's SHA-256 in
  its row; peers talk mutual TLS 1.3 (ring) and accept only the listed
  certificate for the replica at the other end, checking the handshake
  signature against it. A reused pod IP cannot receive a message meant for
  the pod that had it.
- **At most once, in order per publisher.** One connection per peer, one
  frame at a time, each acknowledged; a peer that does not acknowledge
  within `send_timeout` is dropped and dialled afresh. A replica that needs
  what it missed resyncs in its `on_reregister` hook, or from a durable log.
- **A publish reaches the publisher's own subscribers.** In process, before
  the directory is read; there is no separate local path to add beside a
  subscription.
- **Handlers do not block.** They run on the publisher's task (in process)
  or on the peer connection's reader; slow work goes to its own task.
- **Do not use it for wakes.** A processing object's store wakes the
  workers running on it after each commit that makes an object due, and
  the poll covers every other replica (note 81). A nanoservice never sends
  a wake by hand.

## What the tests pin

- `src/lib.rs`: a local bus delivers to every handler and counts the
  replica once; a message carries the replica as its origin; unsubscribing
  stops only that handler; bad channels and oversized messages are refused;
  the advertised address.
- `src/peer.rs`: a publish frame round-trips (up to 3 MiB); bad frames are
  refused; the pin accepts only the listed certificate.
- `tests/bus.rs` (replicas over one database and localhost, as `app`): a
  message reaches every subscribed replica whole (1 MiB); only the
  channel's subscribers receive it, the publisher's own included; a
  publisher's 200 messages arrive in order; a replica that unsubscribes or
  stops is forgotten; one that stopped heartbeating is swept; one swept
  while alive registers again and runs its hook; a peer whose listed
  certificate is not its own is refused, both ways.

## Porting notes

`docs/porting-notes.md` 104 (the rebuild: the directory, the wire, what
went: `publish_tx`, `Delivery`, `deliver_to_self`, `on_reconnect`,
`listening()`, the 8000-byte bound); 63 is the `LISTEN`/`NOTIFY` bus it
replaced; 81 (the processing-object wake is in process).

## File map

| File | Responsibility |
|---|---|
| `src/lib.rs` | `Bus`, `Config`, `Message`, `Handler`, `ReregisterHook`, `Subscription`, `Error`, the constants, `run`'s heartbeat loop |
| `src/directory.rs` | Registration, the subscription rows, heartbeat, sweep, the subscriber lookup, the fingerprint lookup |
| `src/peer.rs` | The identity (rcgen certificate, rustls configurations, the pinning verifiers), the wire, dialling and publishing to a peer, serving the ones that dial in |
| `tests/bus.rs` | The suite across replicas over `TEST_DATABASE_URL` |
