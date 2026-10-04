# basable-pubsub — cross-replica broadcast over Postgres LISTEN/NOTIFY

The port of the monorepo's `golang/lib/pubsub`. A nanoservice project runs
as several identical replicas; the `Bus` lets one replica broadcast a
message that handlers on every replica receive, with Postgres as the
transport and no broker. One `Bus` per process multiplexes every logical
channel over one listen connection and runs as a background task. This is
the BROADCAST channel (SSE fan-out, cross-replica cancels); the
processing-object wake channel is separate, owned by
`basable-processingobject` and the app's `WakeBus`
(`docs/porting-notes.md` 63–64).

The Directive (`docs/DIRECTIVE.md` in every tenant repository,
`golang/controller/lib/scaffold/directive.md` in the monorepo) is the
contract this crate serves.

## The surface

| Item | What it is |
|---|---|
| `Bus::new(pool)` | A bus over the pool; `instance_id()` is its random UUID, the origin prefix on every payload it publishes |
| `subscribe(channel, handler)` | Registers a `Handler` (`Box<dyn Fn(Message) + Send + Sync>`) for a channel, BEFORE `run`; `SubscribeError::{AfterRun, InvalidChannel}` |
| `on_reconnect(hook)` | Registers a `ReconnectHook` run after the listen connection is re-established, before `run` only. Notifications may have been missed during the gap; a subscriber that needs gap recovery (a resync broadcast) registers here |
| `deliver_to_self()` | Receive own messages too (off by default) |
| `publish(channel, data)` | `pg_notify` from the pool with the origin prefix; `PublishError::{PayloadTooLarge, InvalidChannel, Database}` |
| `publish_tx(tx: &mut PgConnection, channel, data, delivery)` | Queues the notification in the caller's transaction: Postgres delivers it only if that transaction commits. `Delivery::ExcludeSelf` (siblings only) or `IncludeSelf` (the nil origin, so the publisher receives it even with dedup on) |
| `run(ctx)` | Listens on every subscribed channel and dispatches until `ctx` is cancelled, reconnecting after `RECONNECT_DELAY` (3 s) on a connection error and firing the hooks after each re-establishment; with no subscriptions it idles |
| `is_listening()`, `listening().await` | Whether the `LISTEN` is in place; a publish before that is silently lost |
| `Message { channel, origin, data }` | A received notification; `data` is the application payload exactly as published |
| `NOTIFY_MAX_BYTES` (8000), `MAX_DATA_BYTES` (8000 − 36 − 64) | Postgres's hard limit and the largest payload `publish` accepts; a bigger payload is truncated by the caller first |

## Rules

- **Subscribe before run.** The channel set is fixed once the connection
  listens; a later `subscribe` is `AfterRun`, not a panic.
- **Handlers do not block.** They run on the bus's listen task; slow work
  goes to its own task.
- **Payloads are text.** A `NOTIFY` payload is a string, so `publish`
  takes `&str` and a `Message` carries a `String`; the wire form is
  `<origin uuid><data>`, and junk without a well-formed origin is dropped.
- **A bus needs a pool.** Go's nil-pool "stub mode" is gone; a test that
  wants no database does not build a bus.
- **Wait for `listening()` in tests.** Go's tests slept; here the signal
  says when the first publish will be heard.
- **Do not use it for wakes.** `basable-app`'s `WakeBus` holds the one
  `LISTEN` on `processing_object_wake`; a nanoservice never publishes a
  wake by hand (the store does, inside the writing transaction).

## What the tests pin

- `src/lib.rs`: the wire payload carries the origin and bounds the data;
  dispatch skips own messages unless asked and tolerates junk; subscribing
  after run or on a bad channel is refused.
- `tests/bus.rs` (two buses over one database): a publish reaches the
  sibling and not the publisher; `deliver_to_self` and `IncludeSelf` reach
  the publisher too; a killed listen connection (its backend terminated)
  comes back and fires the reconnect hook.

## Porting notes

`docs/porting-notes.md` 63 (the Go bus one to one: origin prefix, dedup,
the payload bound, the reconnect delay; added `listening()` and the
`SubscribeError`; reconnection is sqlx's `PgListener`, so
`db.ReleaseListenConn` has no counterpart here), 64 (one wake listener per
process, in `basable-app`).

## File map

| File | Responsibility |
|---|---|
| `src/lib.rs` | `Bus`, `Delivery`, `Message`, `Handler`, `ReconnectHook`, `SubscribeError`, `PublishError`, the constants, `run`'s listen loop |
| `tests/bus.rs` | The two-replica suite over `TEST_DATABASE_URL` |
