# basable-app, basable-db, basable-connect, basable-auth, basable-pubsub, basable-testkit (crates 0.11.0)

- **`basable-app`** — `App::new(config).connect().await` opens one
  `NanoPool<N>` per stateful nanoservice (`SET ROLE nano_<name>`,
  `search_path` pinned), verifies the migration ledger holds every embedded
  version (missing ⇒ exit 3 naming it). `app.serve().connect(..).raw(..)
  .auth(..).components(router).run()` mounts the Connect router and the raw
  routes on one axum server, starts every component's loops, serves
  `/healthz` and `/readyz`, and joins every loop on SIGTERM (naming stuck
  ones, `<component>/<loop>`). A component's loops are its own: it
  implements `basable_app::Component` and lists its workers and tickers in
  `loops()` (a plain executor keeps the default, none); the generated
  messenger implements `Components` over every component in `routing.yaml`.
  Tracing is JSON to stdout.
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
- **`basable-pubsub`** — `Bus` for publish/subscribe across replicas (SSE
  fan-out, cancels), `App::bus()`: `subscribe(channel, handler)` (any
  time; a `Subscription` to `unsubscribe`), `publish(channel, bytes)`
  returning how many replicas took the message, this one's own subscribers
  included, `on_reregister` for gap recovery. Its directory is the
  framework migration `20260101000003_basable_pubsub_directory.sql`; each
  message goes from the publisher straight to every subscribing replica
  over mutual TLS (the Deployment's `POD_IP`), whole up to 16 MiB, at most
  once, in order per publisher. Processing-object workers are not woken
  through it: a worker's wake is in-process, from its own store.
- **`basable-testkit`** — testcontainers Postgres (reused container, a
  database per test), `CommitFaultProxy` (drops one COMMIT ack),
  `test_service_pool::<Svc>()`; `basable-processingobject-testkit` adds the
  conformance type, `Harness`, `Gate`, `Replica`, `drive_once`.
