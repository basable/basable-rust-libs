# basable-processingobject (crates 0.1.0)

The lifecycle framework. A nanoservice declares a type, an adapter and a
reconciler; the framework owns identity, generation, scheduling, retry,
deletion intent and claim authority (the Directive §3–§5).

```rust
pub struct Tx<'c>(&'c mut PgConnection);           // statements only
pub trait Adapter<S, T>: Send + Sync + 'static {   // async fn in trait (RPITIT)
    async fn insert_spec(&self, tx: &mut Tx<'_>, r: &Ref, spec: &S) -> Result<(), sqlx::Error>;
    async fn insert_status(&self, tx: &mut Tx<'_>, r: &Ref, status: &T) -> Result<(), sqlx::Error>;
    async fn read_rows(&self, tx: &mut Tx<'_>, ids: &[Uuid]) -> Result<Vec<Row<S, T>>, sqlx::Error>; // batched
    async fn write_spec(&self, tx: &mut Tx<'_>, r: &Ref, spec: &S) -> Result<(), sqlx::Error>;
    async fn write_status(&self, tx: &mut Tx<'_>, r: &Ref, status: &T) -> Result<(), sqlx::Error>; // whole row
    async fn finalize_delete(&self, tx: &mut Tx<'_>, obj: &Object<S, T>) -> Result<(), sqlx::Error> { Ok(()) }
}
#[must_use] pub enum Outcome<T> {
    Converged { status: Option<T>, schedule: Schedule },
    Retry { status: Option<T>, cause: BoxError },     // Outcome::retry(status, impl Into<BoxError>)
    Blocked { status: Option<T>, cause: BoxError },   // Outcome::blocked(status, impl Into<BoxError>)
    Settled { status: Option<T> },
    Delete,
}
pub enum Schedule { Resync, After(Duration), Now }   // .after()/.requeue_now() only on Converged
impl<S, T: Clone> TypedStore<S, T> {
    pub async fn bind<N: Stateful>(pool: &NanoPool<N>, decl: ProcessingObjectType<S, T>) -> Result<Self, Error>;
    pub async fn create(&self, id: Uuid, name: NamespacedName, spec: S, status: T, opts: CreateOptions) -> Result<Ref, Error>;
    pub async fn update_spec<F>(&self, r: &Ref, mutate: F) -> Result<(), Error>
        where F: FnOnce(&mut S, &T) -> Result<(), AppError> + Send;   // sync: no I/O under the lock; a refusal is the caller's error
    pub async fn mark_deleted(&self, r: &Ref) -> Result<bool, Error>;
    pub async fn nudge(&self, r: &Ref) -> Result<(), Error>;
    pub async fn read_many(&self, ids: &[Uuid]) -> Result<Vec<Object<S, T>>, Error>;
}
impl<S, T: Clone> Claim<S, T> {
    pub fn adopted(&self) -> bool;
    pub async fn heartbeat(&self) -> Result<(), Error>;
    pub async fn write_status(&mut self, status: T) -> Result<(), Error>;   // declare-before-I/O only
}
pub trait Reconciler<S, T>: Send + Sync {
    fn reconcile(&self, ctx: &Ctx, claim: &mut Claim<S, T>) -> impl Future<Output = Result<Outcome<T>, BoxError>> + Send;
}
```

Errors: an adapter's are `sqlx::Error` (a column mapper fails no other way);
a reconciler's `Err` is any `BoxError` and resolves to `Retry { status: None
}`; `Outcome::retry` and `Outcome::blocked` take `impl Into<BoxError>`, so a
`&str` reason works. A typed `Error` from the store is the framework's own
enum (an unknown id, a lost claim, the database).

`WorkerConfig` fields and defaults: resync 10m, backoff 5s/5m, max_attempts
0 (unbounded), attempt_timeout 5m, poll 30s, batch 50, parallelism 1,
after_complete 5s, `label_selector`. The scheduling fields must be identical
on every replica. Constants: lease slack 30s, completion timeout 30s,
deleting alarm 30m, `last_error` cut at 4000 bytes.

Scheduling is poll-first; the wake only shortens latency, and it is
in-process. A worker takes the wakes of the store it was built on while it
runs: a committed `create`, `update_spec`, `mark_deleted` or `nudge` through
that store, and a completion that leaves its object due now
(`requeue_now`, a superseded or woken pass), start a scan at once; a
completion due again within one poll interval (`after(d)`, a retry backoff)
starts one on a timer. A worker with no free slot (`parallelism`) claims
nothing until a slot frees or the poll runs.

Per type the scaffolder renders `types/<type>/{type,adapter,reconciler}.rs`
and a migration with the registry row, the partition
`nano_<name>.processing_object_<type> PARTITION OF basable.processing_object`,
and the `<type>_spec` / `<type>_status` tables with a composite FK to the
envelope. The framework's SQL targets the partition, which is what makes
GRANT-based ownership work.
