# basable-app, basable-db, basable-connect, basable-auth, basable-pubsub, basable-testkit (crates 0.1.0)

- **`basable-app`** — `App::new(config).connect().await` opens one
  `NanoPool<N>` per stateful nanoservice (`SET ROLE nano_<name>`,
  `search_path` pinned), verifies the migration ledger holds every embedded
  version (missing ⇒ exit 3 naming it), starts the `WakeBus` (one LISTEN
  connection per process fanning `processing_object_wake` to per-type
  subscriptions). `app.serve(router).connect(..).raw(..).auth(..)
  .worker(name, w).ticker(name, t).run()` mounts the Connect router and the
  raw routes on one axum server, registers workers and tickers, serves
  `/healthz` and `/readyz`, and joins every loop on SIGTERM (naming stuck
  workers). Tracing is JSON to stdout.
- **`basable-db`** — `Nanoservice`/`Stateful` markers, `NanoPool<N>` (a pool
  for a nanoservice that owns nothing is a compile error), `MigratorPool`
  (tests and local bootstrap only), the dbmate-format migration runner the
  testkit applies, SQLSTATE helpers (23505 ⇒ `is_unique_violation`).
- **`basable-connect`** — the glue over `connectrpc`: `AppError` ↔ Connect
  codes, request-id/tracing interceptors, `ConnectRouter::into_axum_service`.
- **`basable-auth`** — `Validator::kratos(url)`: a tower layer calling
  `/sessions/whoami` with the request's cookie; public-path allow-lists; a
  `SessionHook` after every validation; `ctx.user_id()` in handlers. A test
  bypass exists only through the testkit.
- **`basable-pubsub`** — `Bus` over LISTEN/NOTIFY for cross-replica
  broadcast (SSE fan-out, cancels): publish, subscribe-before-run,
  own-message dedup, ~8 KB payload bound. Distinct from the wake channel.
- **`basable-testkit`** — testcontainers Postgres (reused container, a
  database per test), `CommitFaultProxy` (drops one COMMIT ack),
  `test_service_pool::<Svc>()`; `basable-processingobject-testkit` adds the
  conformance type, `Harness`, `Gate`, `Replica`, `drive_once`.
