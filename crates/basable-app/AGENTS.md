# basable-app — the runtime a nanoservice binary is assembled on

There is no single Go original: this crate is what the monorepo's
`golang/controller/main.go` wiring and `golang/controller/lib/server`'s
lifecycle (workers, health, graceful shutdown) do, shaped for a tenant's
`app/src/main.rs` so that file is a dozen lines of composition and nothing
else (`docs/porting-notes.md` 63–68 list what Rust changed, 81 why there
is no wake listener). The Directive (`docs/DIRECTIVE.md` in every tenant
repository, `golang/controller/lib/scaffold/directive.md` in the monorepo)
is the contract this crate serves: workers are registered, never spawned loose;
state lives in Postgres; a boot that cannot prove its migrations ran does
not serve.

```text
basable_app::tracing::init_json();
let cfg = config::Config::load()?;                       // flattens basable_app::Config
let app = App::new(cfg.app.clone()).expect_migrations(MIGRATIONS).connect().await?;
let catalog = Catalog::new(&app, app.pool::<catalog::Schema>().await?).await;
let router: &'static AppMessenger = Box::leak(Box::new(AppMessenger::new(api, catalog, ..)));
app.serve()
    .connect(api.connect_router(router))
    .raw(api.raw_routes(router))
    .auth(Validator::kratos(&cfg.kratos_public_url))
    .worker("catalog", catalog::types::product::worker(router, router.catalog()))
    .ticker("catalog", catalog::worker::tickers(router, router.catalog()))
    .run()
    .await
```

## The surface

| Item | What it does |
|---|---|
| `Config` (`from_env`, `new(url)`, `validate`) | The framework's variables (`var::*` names them): `DATABASE_URL`; `HOST` (default `0.0.0.0`), `PORT` (8080), `DATABASE_POOL_MAX_CONNECTIONS` (8 per nanoservice pool), `DATABASE_CONNECTION_BUDGET` (100 across every pool), `DATABASE_BOOT_WAIT_SECS` (60), `SHUTDOWN_GRACE_SECS` (30). `serde::Deserialize` with defaults, so a project's own config flattens it |
| `App::new(cfg)` → `Unbooted` | `.expect_migrations(&["<version>", ..])`, `.connect_options(..)`, then `.connect()`: the framework pool (`APP_POOL_CONNECTIONS`, 3: the ledger check, the pubsub listener and its publishes, the readiness probe) with a bounded wait — a refused connection or a ledger missing a version is retried every second until the boot wait, then the last error is the verdict (`Error::Connect` / `Error::Migrations`) |
| `App::pool::<N: Stateful>()` | One `basable_db::NanoPool<N>` per stateful nanoservice, under the connection budget: opening one more than the budget allows is `Error::ConnectionBudget` at boot, not a surprise at the database's `max_connections` (`connections_reserved` says how much is spoken for) |
| `App::bus()`, `App::framework_pool()` | The `basable_pubsub::Bus`, the framework's own pool |
| `App::serve()` → `Serve` | `.connect(ConnectRouter)`, `.raw(axum::Router)`, `.auth(Validator)` (the layer over every route; `/healthz` and `/readyz` are public from the start), `.worker(name, Worker)` (a worker is woken in process by writes through the store it was built on, so the app runs no wake listener and the budget counts none — note 81), `.ticker(name, tickers)`; `.start()` → `Running` (`addr`, `ctx`, `is_ready`, `shutdown`), `.run()` = start, wait for SIGTERM or SIGINT, shut down |
| `Ticker::new(name, every, \|ctx\| Box::pin(..))` | An immediate first tick, then one per interval; a failing tick is logged and retried next time; joined on shutdown. Never a CronJob |
| `basable_app::tracing::init_json()` / `init_pretty()` | JSON logs for production, pretty for a terminal |
| `Error` | `DatabaseUrl`, `Connect { attempts, source }`, `Migrations`, `Pool { nanoservice, source }`, `ConnectionBudget { requested, budget }`, `Bind`, `Server`, `Stuck(names)` |

Re-exported for `main.rs`: `Validator`, `Identity`, `AuthCtx`
(basable-auth), `ConnectRouter`, `request_ctx` (basable-connect), `Bus`.

## Probes and shutdown

`/healthz` answers `ok` while the process lives. `/readyz` is three checks
(note 68): wiring finished (`Serve::start` returned), `SELECT 1` on the
framework pool within two seconds, and every registered loop still alive —
a worker or ticker task that ended flips its flag and readiness reports
`not ready: worker loop(s) ended: <names>`. Liveness stays green, so
Kubernetes stops routing to the pod without restarting it.

`shutdown()` turns readiness off, cancels the root `Ctx`, and joins every
loop within the grace: an in-flight reconcile attempt sees its context
cancelled and completes as a retry, a tick ends at its next await, the
server finishes its requests. A loop that does not drain is named and
abandoned (`Error::Stuck`), never waited on forever.

## Rules for a consumer

- Every worker and ticker goes through `Serve`; a loose `tokio::spawn`
  is invisible to readiness and to shutdown.
- `expect_migrations` lists the project's migration versions (the
  scaffolder generates `migrations::MIGRATIONS` from `db/app/migrations`);
  a binary whose ledger is behind does not serve.
- Open nanoservice pools through `App::pool`, never a second `PgPool`:
  the budget only counts what it sees.

## Tests

`tests/app.rs`, over a real database (`TEST_DATABASE_URL`): boot refuses
a ledger behind the binary and accepts a current one; a database that
never answers fails within the boot wait; nanoservice pools are isolated
and budgeted; readiness flips after wiring and the routes are served; a
write through a registered worker's store wakes it under a long poll, and
shutdown drains its attempt as a retry.

## File map

| File | Responsibility |
|---|---|
| `src/config.rs` | `Config`, `ConfigError`, the `var` names and defaults |
| `src/boot.rs` | `Unbooted`, `App`, the bounded connect, the ledger gate, the budget |
| `src/serve.rs` | `Serve`, `Running`, the probes, the loop registry, shutdown |
| `src/ticker.rs` | `Ticker`, `TickFuture` |
| `src/logging.rs` (`basable_app::tracing`) | `init_json`, `init_pretty` |
| `src/error.rs` | `Error` |
