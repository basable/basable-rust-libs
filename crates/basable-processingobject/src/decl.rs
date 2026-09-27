//! A processing-object type's declaration — its identity in the type
//! registry and the [`Adapter`] through which the framework reads and
//! writes the type's spec and status tables — and [`WorkerConfig`], the
//! reconcile and runtime policy supplied where the type's worker is
//! registered. The declaration is a constant; everything tunable lives in
//! the config.
//!
//! The adapter is a dumb column mapper, the payoff of invariant 3: every
//! status write the framework issues is fenced identically, so the adapter
//! never needs to know who is writing — no provenance flags, no conditional
//! guards, no monotonic merges.

use std::future::Future;
use std::time::Duration;

use basable_core::labels::{Labels, validate_labels};
use basable_core::names::{validate_public_id_prefix, validate_type_name};
use uuid::Uuid;

use crate::error::Error;
use crate::model::{Object, Ref, Row};
use crate::tx::Tx;

/// Maps one type's spec and status onto its typed tables. Every method runs
/// inside a framework-owned transaction that has already locked the
/// envelope row; methods are database-only (invariant 7), never retain the
/// [`Tx`], and never touch envelope columns (invariant 1). The Go original
/// checked five function fields for nil at bind; here a missing one does
/// not compile.
pub trait Adapter<S, T>: Send + Sync + 'static {
    /// Inserts the typed spec row at create, in the envelope's transaction.
    fn insert_spec(
        &self,
        tx: &mut Tx<'_>,
        r: &Ref,
        spec: &S,
    ) -> impl Future<Output = Result<(), sqlx::Error>> + Send;

    /// Inserts the initial typed status row at create. Every object has a
    /// status row from birth; completion updates, never inserts.
    fn insert_status(
        &self,
        tx: &mut Tx<'_>,
        r: &Ref,
        status: &T,
    ) -> impl Future<Output = Result<(), sqlx::Error>> + Send;

    /// Loads the typed rows for the given ids in one call — batched by
    /// contract, once per claim batch or read, never per row. Every
    /// requested id has both typed rows; a missing one is an invariant
    /// violation the framework surfaces.
    fn read_rows(
        &self,
        tx: &mut Tx<'_>,
        ids: &[Uuid],
    ) -> impl Future<Output = Result<Vec<Row<S, T>>, sqlx::Error>> + Send;

    /// Overwrites the typed spec row for an accepted mutation, in the
    /// transaction that advances the generation (invariant 2).
    fn write_spec(
        &self,
        tx: &mut Tx<'_>,
        r: &Ref,
        spec: &S,
    ) -> impl Future<Output = Result<(), sqlx::Error>> + Send;

    /// Overwrites the typed status row. Single provenance: the framework
    /// calls it only from paths holding exact claim authority, so the
    /// adapter writes every column it owns, unconditionally.
    fn write_status(
        &self,
        tx: &mut Tx<'_>,
        r: &Ref,
        status: &T,
    ) -> impl Future<Output = Result<(), sqlx::Error>> + Send;

    /// Runs inside the deletion completion after every fence passes and
    /// before the envelope row is deleted: the place for durable teardown
    /// evidence that outlives the object. Must be idempotent — an ambiguous
    /// commit retries it (invariant 5). Optional; does nothing by default.
    fn finalize_delete(
        &self,
        tx: &mut Tx<'_>,
        obj: &Object<S, T>,
    ) -> impl Future<Output = Result<(), sqlx::Error>> + Send {
        let _ = (tx, obj);
        async { Ok(()) }
    }
}

/// One reconciled type: registry identity plus the typed-table adapter,
/// nothing else. It is a constant — no field varies by environment or
/// replica; policy lives in [`WorkerConfig`].
#[derive(Debug, Clone)]
pub struct ProcessingObjectType<S, T, A: Adapter<S, T>> {
    /// The registry name, matching the type's row in
    /// `processing_object_type` and the partition `processing_object_<name>`
    /// in the nanoservice's schema. Lowercase snake_case, at most 64
    /// characters.
    pub name: &'static str,
    /// The `SMALLINT` registry key and envelope partition value. Positive,
    /// unique across the deployment.
    pub type_key: i16,
    /// The prefix of the type's public ids (`external_id` is
    /// `publicid::encode(prefix, id)`). Lowercase letters only.
    pub public_id_prefix: &'static str,
    /// The typed-table mapper.
    pub adapter: A,
    #[doc(hidden)]
    pub _marker: std::marker::PhantomData<fn() -> (S, T)>,
}

impl<S, T, A: Adapter<S, T>> ProcessingObjectType<S, T, A> {
    /// A declaration.
    pub fn new(
        name: &'static str,
        type_key: i16,
        public_id_prefix: &'static str,
        adapter: A,
    ) -> ProcessingObjectType<S, T, A> {
        ProcessingObjectType {
            name,
            type_key,
            public_id_prefix,
            adapter,
            _marker: std::marker::PhantomData,
        }
    }

    /// Checks the declaration's contract: the store calls it at bind and
    /// nothing touches an unvalidated declaration.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        validate_type_name(self.name).map_err(|e| Error::invalid(format!("type {}", e)))?;
        if self.type_key <= 0 {
            return Err(Error::invalid(format!(
                "type {:?}: type_key must be positive, got {}",
                self.name, self.type_key
            )));
        }
        validate_public_id_prefix(self.public_id_prefix)
            .map_err(|e| Error::invalid(format!("type {:?}: public_id_prefix {}", self.name, e)))?;
        Ok(())
    }

    /// The partition table of this type in its nanoservice's schema. The
    /// name passed validation, so it is safe to interpolate.
    pub(crate) fn partition(&self) -> String {
        format!("processing_object_{}", self.name)
    }
}

/// The retry schedule for a type: exponential from `base`, capped at `max`,
/// with deterministic jitter derived from the object identity and attempt.
/// Determinism is deliberate — every replica computes the identical
/// schedule, tests reproduce it exactly, and no wall clock or RNG leaks into
/// scheduling. `delay` is bit-identical to the Go original (a golden table
/// pins it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// The first delay.
    pub base: Duration,
    /// The cap.
    pub max: Duration,
}

impl Backoff {
    /// The wait before the given attempt (1-based) of the given generation.
    pub fn delay(&self, id: Uuid, generation: i64, attempt: i32) -> Duration {
        let attempt = attempt.max(1);
        let shift = (attempt - 1).min(20) as u32;
        // Go: `b.Base << shift`, an int64 that can overflow negative; then
        // `delay > Max || delay <= 0` takes the cap.
        let base = self.base.as_nanos() as i64;
        let max = self.max.as_nanos() as i64;
        let shifted = base.wrapping_shl(shift);
        let delay = if shifted > max || shifted <= 0 {
            max
        } else {
            shifted
        };
        // Deterministic jitter in [75%, 100%) of the exponential delay, keyed
        // by id, generation and attempt so distinct objects spread out.
        let mut h = Fnv64a::new();
        h.write(id.as_bytes());
        h.write(format!("/{generation}/{attempt}").as_bytes());
        let factor = 0.75 + 0.25 * ((h.finish() % 1000) as f64) / 1000.0;
        Duration::from_nanos((delay as f64 * factor) as u64)
    }
}

/// FNV-1a, 64-bit, as Go's `hash/fnv` `New64a`.
struct Fnv64a(u64);

impl Fnv64a {
    fn new() -> Fnv64a {
        Fnv64a(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// The reconcile and runtime policy for one type, supplied where its worker
/// is registered. The scheduling fields — `resync`, `backoff`,
/// `max_attempts`, `attempt_timeout` — are written into shared envelope
/// state or anchor the claim lease, so every replica of one logical worker
/// (one distinct `label_selector`; the whole type when empty) must run
/// identical values. `poll_interval`, `batch_size`, `parallelism` and
/// `after_complete_timeout` are per-replica tuning and may differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerConfig {
    /// The default schedule after a plain `Converged`. Default 10 minutes.
    pub resync: Duration,
    /// The retry schedule. Default 5 s base, 5 min cap.
    pub backoff: Backoff,
    /// Escalates to `Blocked` after this many consecutive failed attempts at
    /// one generation; zero means unbounded. Never applies to a deleting
    /// object.
    pub max_attempts: u32,
    /// Bounds one attempt and anchors the claim lease. Default 5 minutes.
    pub attempt_timeout: Duration,
    /// The claim loop's scan cadence; wakes are a latency hint on top.
    /// Default 30 seconds.
    pub poll_interval: Duration,
    /// How many due objects one claim transaction takes. Default 50.
    pub batch_size: u32,
    /// How many claimed objects one replica reconciles concurrently.
    /// Default 1.
    pub parallelism: u32,
    /// Bounds the post-completion callback. Default 5 seconds.
    pub after_complete_timeout: Duration,
    /// Restricts claiming to objects whose labels contain every listed pair
    /// (JSONB containment). Empty means unfiltered. A non-empty selector
    /// defines a logical worker; the owning nanoservice keeps its selectors
    /// pairwise disjoint and jointly covering.
    pub label_selector: Labels,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        WorkerConfig {
            resync: Duration::from_secs(10 * 60),
            backoff: Backoff {
                base: Duration::from_secs(5),
                max: Duration::from_secs(5 * 60),
            },
            max_attempts: 0,
            attempt_timeout: Duration::from_secs(5 * 60),
            poll_interval: Duration::from_secs(30),
            batch_size: 50,
            parallelism: 1,
            after_complete_timeout: Duration::from_secs(5),
            label_selector: Labels::new(),
        }
    }
}

impl WorkerConfig {
    /// The config with every zero field replaced by its default, or the
    /// first contract violation. Go's field types admitted negatives; here
    /// they are unsigned, and a zero means "default".
    pub fn validated(mut self) -> Result<WorkerConfig, Error> {
        validate_labels(&self.label_selector)
            .map_err(|e| Error::invalid(format!("label_selector: {e}")))?;
        let d = WorkerConfig::default();
        if self.resync.is_zero() {
            self.resync = d.resync;
        }
        if self.backoff.base.is_zero() {
            self.backoff.base = d.backoff.base;
        }
        if self.backoff.max < self.backoff.base {
            self.backoff.max = d.backoff.max.max(self.backoff.base);
        }
        if self.poll_interval.is_zero() {
            self.poll_interval = d.poll_interval;
        }
        if self.attempt_timeout.is_zero() {
            self.attempt_timeout = d.attempt_timeout;
        }
        if self.batch_size == 0 {
            self.batch_size = d.batch_size;
        }
        if self.parallelism == 0 {
            self.parallelism = d.parallelism;
        }
        if self.after_complete_timeout.is_zero() {
            self.after_complete_timeout = d.after_complete_timeout;
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    impl Adapter<String, String> for Noop {
        async fn insert_spec(
            &self,
            _: &mut Tx<'_>,
            _: &Ref,
            _: &String,
        ) -> Result<(), sqlx::Error> {
            Ok(())
        }
        async fn insert_status(
            &self,
            _: &mut Tx<'_>,
            _: &Ref,
            _: &String,
        ) -> Result<(), sqlx::Error> {
            Ok(())
        }
        async fn read_rows(
            &self,
            _: &mut Tx<'_>,
            _: &[Uuid],
        ) -> Result<Vec<Row<String, String>>, sqlx::Error> {
            Ok(Vec::new())
        }
        async fn write_spec(&self, _: &mut Tx<'_>, _: &Ref, _: &String) -> Result<(), sqlx::Error> {
            Ok(())
        }
        async fn write_status(
            &self,
            _: &mut Tx<'_>,
            _: &Ref,
            _: &String,
        ) -> Result<(), sqlx::Error> {
            Ok(())
        }
    }

    fn widget() -> ProcessingObjectType<String, String, Noop> {
        ProcessingObjectType::new("widget", 1, "wdg", Noop)
    }

    #[test]
    fn declarations_validate() {
        assert!(widget().validate().is_ok());
        assert_eq!(widget().partition(), "processing_object_widget");
        for (name, key, prefix) in [
            ("Widget", 1, "wdg"),
            ("widget", 0, "wdg"),
            ("widget", 1, "W"),
        ] {
            let d = ProcessingObjectType::<String, String, _>::new(name, key, prefix, Noop);
            assert!(
                matches!(d.validate(), Err(Error::InvalidConfig(_))),
                "{name} {key} {prefix}"
            );
        }
    }

    #[test]
    fn the_zero_config_gets_every_default() {
        let cfg = WorkerConfig {
            resync: Duration::ZERO,
            backoff: Backoff {
                base: Duration::ZERO,
                max: Duration::ZERO,
            },
            attempt_timeout: Duration::ZERO,
            poll_interval: Duration::ZERO,
            batch_size: 0,
            parallelism: 0,
            after_complete_timeout: Duration::ZERO,
            ..WorkerConfig::default()
        }
        .validated()
        .unwrap();
        assert_eq!(cfg, WorkerConfig::default());
        assert_eq!(cfg.resync, Duration::from_secs(600));
        assert_eq!(cfg.backoff.max, Duration::from_secs(300));
        assert_eq!(cfg.batch_size, 50);

        // A cap below the base is raised to the base.
        let cfg = WorkerConfig {
            backoff: Backoff {
                base: Duration::from_secs(600),
                max: Duration::from_secs(1),
            },
            ..WorkerConfig::default()
        }
        .validated()
        .unwrap();
        assert_eq!(cfg.backoff.max, Duration::from_secs(600));

        // Explicit values survive.
        let cfg = WorkerConfig {
            parallelism: 4,
            batch_size: 7,
            ..WorkerConfig::default()
        }
        .validated()
        .unwrap();
        assert_eq!((cfg.parallelism, cfg.batch_size), (4, 7));

        let bad = WorkerConfig {
            label_selector: [("Bad".to_string(), "x".to_string())].into_iter().collect(),
            ..WorkerConfig::default()
        };
        assert!(matches!(bad.validated(), Err(Error::InvalidConfig(_))));
    }

    #[test]
    fn backoff_grows_exponentially_with_keyed_jitter() {
        let b = Backoff {
            base: Duration::from_secs(1),
            max: Duration::from_secs(60),
        };
        let id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let within = |d: Duration, tier: Duration| {
            assert!(
                d >= Duration::from_secs_f64(tier.as_secs_f64() * 0.75),
                "{d:?} < 75% of {tier:?}"
            );
            assert!(d < tier, "{d:?} >= {tier:?}");
        };
        within(b.delay(id, 1, 1), Duration::from_secs(1));
        within(b.delay(id, 1, 2), Duration::from_secs(2));
        within(b.delay(id, 1, 4), Duration::from_secs(8));
        within(b.delay(id, 1, 40), Duration::from_secs(60));
        assert_eq!(
            b.delay(id, 1, 0),
            b.delay(id, 1, 1),
            "attempts below 1 read as 1"
        );

        // Keyed spread: distinct ids and generations at one tier differ.
        let ids: Vec<Uuid> = (0u8..16)
            .map(|i| Uuid::new_v5(&Uuid::nil(), &[i]))
            .collect();
        let seen: std::collections::HashSet<Duration> =
            ids.iter().map(|id| b.delay(*id, 1, 3)).collect();
        assert!(seen.len() > 1);
        assert_ne!(b.delay(ids[0], 1, 3), b.delay(ids[0], 2, 3));
    }

    /// The Go implementation's delays for 405 (policy, id, generation,
    /// attempt) tuples, generated 2026-09-27 from the monorepo's
    /// `Backoff.Delay`. Bit-identical means the same FNV-1a keying and the
    /// same float arithmetic.
    #[test]
    fn backoff_is_bit_identical_to_go() {
        let table = include_str!("../tests/fixtures/backoff_go.txt");
        let mut rows = 0;
        for line in table.lines().filter(|l| !l.trim().is_empty()) {
            let f: Vec<&str> = line.split_whitespace().collect();
            let b = Backoff {
                base: Duration::from_nanos(f[0].parse().unwrap()),
                max: Duration::from_nanos(f[1].parse().unwrap()),
            };
            let id = Uuid::parse_str(f[2]).unwrap();
            let generation: i64 = f[3].parse().unwrap();
            let attempt: i32 = f[4].parse().unwrap();
            let want = Duration::from_nanos(f[5].parse().unwrap());
            assert_eq!(b.delay(id, generation, attempt), want, "{line}");
            rows += 1;
        }
        assert_eq!(rows, 405);
    }
}
