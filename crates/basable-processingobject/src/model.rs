//! Identity and the read model (invariant 1): what every object carries and
//! what a read returns. Nothing here is claim authority — holding a
//! [`Meta`] grants nothing; the claim token is deliberately not exposed.

use std::fmt;

use basable_core::labels::Labels;
use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

use crate::error::Error;

/// Scheduling sentinel: the epoch, which sorts before every real schedule —
/// due since forever. Every object carries it between create and its first
/// completion.
pub const SCHEDULE_IMMEDIATE: DateTime<Utc> = DateTime::UNIX_EPOCH;

/// Scheduling sentinel: 2900-01-01, which sorts after every real schedule
/// and is excluded from the claim scan index — out of scheduling entirely
/// until new intent or a nudge pulls it back.
pub fn schedule_parked() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2900, 1, 1, 0, 0, 0)
        .single()
        .expect("a fixed valid date")
}

/// [`schedule_parked`] as the SQL literal the claim scan compares against.
/// A bind parameter would defeat the partial index's implication, so the
/// query carries the literal verbatim; `SCHEDULE_PARKED` and this string
/// must agree (a unit test pins it).
pub const SCHEDULE_PARKED_SQL: &str = "'2900-01-01 00:00:00+00'::timestamptz";

/// The parked sentinel, for comparisons ([`Meta::parked`]).
pub static SCHEDULE_PARKED: std::sync::LazyLock<DateTime<Utc>> =
    std::sync::LazyLock::new(schedule_parked);

/// The caller-facing identity every object carries: a human-readable name
/// scoped to a configuration namespace. `namespace` holds the id of the
/// config database's namespace object, a logical reference no foreign key
/// enforces. Both fields are required at create; the pair is unique within
/// its type while the object lives and immutable after create.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NamespacedName {
    /// The configuration namespace's id.
    pub namespace: Uuid,
    /// The name, unique within the namespace and type among live objects.
    pub name: String,
}

impl NamespacedName {
    /// A pair.
    pub fn new(namespace: Uuid, name: impl Into<String>) -> NamespacedName {
        NamespacedName {
            namespace,
            name: name.into(),
        }
    }

    /// Requires a fully-set pair with a name the envelope column can hold.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.name.is_empty() || self.namespace.is_nil() {
            return Err(Error::invalid(format!(
                "envelope identity requires both namespace and name, got {self}"
            )));
        }
        if self.name.chars().count() > 255 {
            return Err(Error::invalid("envelope name exceeds 255 characters"));
        }
        Ok(())
    }
}

impl fmt::Display for NamespacedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.namespace, self.name)
    }
}

/// Names one object: the processing-object type it belongs to and its
/// stable framework identity. Ids are minted client-side at create so an
/// ambiguous commit can be adopted by retrying the same id (invariant 5).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ref {
    /// The type's registry name.
    pub processing_object_type: String,
    /// The object's id.
    pub id: Uuid,
}

impl Ref {
    /// A reference.
    pub fn new(processing_object_type: impl Into<String>, id: Uuid) -> Ref {
        Ref {
            processing_object_type: processing_object_type.into(),
            id,
        }
    }

    /// Requires a named type and a non-nil id.
    pub fn validate(&self) -> Result<(), Error> {
        if self.processing_object_type.is_empty() {
            return Err(Error::invalid("empty processing object type in reference"));
        }
        if self.id.is_nil() {
            return Err(Error::invalid("nil id in object reference"));
        }
        Ok(())
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.processing_object_type, self.id)
    }
}

/// The framework's coarse view of the observed generation: bookkeeping for
/// operators and dashboards, never an input to domain logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// No completion has observed the current generation yet.
    Pending,
    /// The last attempt failed transiently and is scheduled again.
    Retrying,
    /// The last completion succeeded for the generation it observed.
    Converged,
    /// Retries are exhausted or the reconciler declared the intent
    /// unsatisfiable; parked until a new generation or a nudge.
    Blocked,
}

impl Phase {
    /// The column value.
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Pending => "pending",
            Phase::Retrying => "retrying",
            Phase::Converged => "converged",
            Phase::Blocked => "blocked",
        }
    }

    /// The phase for a column value.
    pub fn parse(s: &str) -> Option<Phase> {
        match s {
            "pending" => Some(Phase::Pending),
            "retrying" => Some(Phase::Retrying),
            "converged" => Some(Phase::Converged),
            "blocked" => Some(Phase::Blocked),
            _ => None,
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The envelope's read model: a snapshot, every field true in one
/// consistent transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// The public id: the type's prefix plus the base62 encoding of the id,
    /// derived once at create and unique per type by construction.
    pub external_id: String,
    /// The caller-facing identity, immutable after create.
    pub name: NamespacedName,
    /// Optional discovery metadata, set at create and immutable; empty means
    /// none (stored as NULL).
    pub labels: Labels,
    /// The monotonic desired-state revision; every accepted spec mutation
    /// advances it (invariant 2).
    pub generation: i64,
    /// The database-clock time the current generation was accepted: the age
    /// of unsatisfied intent, which observed-state churn never moves.
    pub generation_changed_at: DateTime<Utc>,
    /// The monotonic wake fence: every accepted mutation and every nudge
    /// advances it; it never resets.
    pub wake_seq: i64,
    /// The newest generation a completion has processed.
    pub observed_generation: i64,
    /// The coarse phase.
    pub phase: Phase,
    /// Consecutive failed attempts at the current generation.
    pub attempts: i32,
    /// The last attempt's failure, bounded by the store; empty after a
    /// successful completion.
    pub last_error: String,
    /// The one-way deletion request (invariant 6).
    pub deleted_at: Option<DateTime<Utc>>,
    /// The scheduling horizon: a finite schedule or one of the two
    /// sentinels.
    pub next_reconcile_at: DateTime<Utc>,
    /// The completion time of the newest committed attempt, the fairness key
    /// of the claim scan; also advanced when an expired claim is adopted.
    pub last_reconciled_at: Option<DateTime<Utc>>,
    /// The acquisition time of the live claim, diagnostic only.
    pub claimed_at: Option<DateTime<Utc>>,
    /// The live claim's lease horizon; expiry never fences the holder by
    /// itself.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// When the envelope was created.
    pub created_at: DateTime<Utc>,
}

impl Meta {
    /// Whether the newest completion has processed the current generation.
    pub fn observed_current(&self) -> bool {
        self.observed_generation >= self.generation
    }

    /// Whether deletion has been requested (invariant 6).
    pub fn deleting(&self) -> bool {
        self.deleted_at.is_some()
    }

    /// Whether the object is out of scheduling entirely until new intent or
    /// a nudge re-arms it. At or past the sentinel counts, so a rounded or
    /// hand-written near-sentinel is still parked.
    pub fn parked(&self) -> bool {
        self.next_reconcile_at >= *SCHEDULE_PARKED
    }
}

/// The adapter's unit of typed data: the spec and status columns for one
/// object, keyed by the envelope identity. Adapters read and write rows;
/// they never see envelope columns (invariant 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row<S, T> {
    /// The object's id.
    pub id: Uuid,
    /// The desired state.
    pub spec: S,
    /// The observed state.
    pub status: T,
}

/// The full read model: envelope snapshot plus typed spec and status, all
/// from one consistent transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object<S, T> {
    /// The envelope.
    pub meta: Meta,
    /// The object's id.
    pub id: Uuid,
    /// The desired state.
    pub spec: S,
    /// The observed state.
    pub status: T,
}

impl<S, T> Object<S, T> {
    /// The object's reference under the given type name. The type is a
    /// parameter because an object does not store it: typed values are only
    /// ever obtained through a type's store, which knows its name.
    pub fn r#ref(&self, processing_object_type: &str) -> Ref {
        Ref::new(processing_object_type, self.id)
    }

    /// [`Meta::observed_current`].
    pub fn observed_current(&self) -> bool {
        self.meta.observed_current()
    }

    /// [`Meta::deleting`].
    pub fn deleting(&self) -> bool {
        self.meta.deleting()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(next: DateTime<Utc>) -> Meta {
        Meta {
            external_id: "wdg_x".into(),
            name: NamespacedName::new(Uuid::from_u128(1), "n"),
            labels: Labels::new(),
            generation: 1,
            generation_changed_at: Utc::now(),
            wake_seq: 0,
            observed_generation: 0,
            phase: Phase::Pending,
            attempts: 0,
            last_error: String::new(),
            deleted_at: None,
            next_reconcile_at: next,
            last_reconciled_at: None,
            claimed_at: None,
            lease_expires_at: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn the_parked_sql_literal_and_the_sentinel_agree() {
        let text = SCHEDULE_PARKED_SQL
            .trim_end_matches("::timestamptz")
            .trim_matches('\'');
        let parsed = DateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%#z").unwrap();
        assert_eq!(parsed.with_timezone(&Utc), *SCHEDULE_PARKED);
        assert!(SCHEDULE_IMMEDIATE < *SCHEDULE_PARKED);
    }

    #[test]
    fn schedule_helpers() {
        assert!(meta(*SCHEDULE_PARKED).parked());
        assert!(
            meta(*SCHEDULE_PARKED + chrono::Duration::hours(1)).parked(),
            "past the sentinel still counts as parked"
        );
        assert!(!meta(Utc::now()).parked());
        assert!(!meta(SCHEDULE_IMMEDIATE).parked());

        let mut m = meta(Utc::now());
        m.generation = 3;
        m.observed_generation = 3;
        assert!(m.observed_current());
        m.observed_generation = 4;
        assert!(m.observed_current());
        m.observed_generation = 2;
        assert!(!m.observed_current());
    }

    #[test]
    fn refs_and_names_validate_and_print() {
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let r = Ref::new("widget", id);
        assert!(r.validate().is_ok());
        assert_eq!(r.to_string(), "widget/11111111-2222-3333-4444-555555555555");
        assert!(matches!(
            Ref::new("", id).validate(),
            Err(Error::InvalidConfig(_))
        ));
        assert!(matches!(
            Ref::new("widget", Uuid::nil()).validate(),
            Err(Error::InvalidConfig(_))
        ));

        assert!(NamespacedName::new(id, "alpha").validate().is_ok());
        assert!(NamespacedName::new(id, "x".repeat(255)).validate().is_ok());
        for bad in [
            NamespacedName::new(Uuid::nil(), ""),
            NamespacedName::new(Uuid::nil(), "alpha"),
            NamespacedName::new(id, ""),
            NamespacedName::new(id, "x".repeat(256)),
        ] {
            assert!(
                matches!(bad.validate(), Err(Error::InvalidConfig(_))),
                "{bad}"
            );
        }
        assert_eq!(
            NamespacedName::new(id, "alpha").to_string(),
            "11111111-2222-3333-4444-555555555555/alpha"
        );
    }

    #[test]
    fn phases_round_trip() {
        for p in [
            Phase::Pending,
            Phase::Retrying,
            Phase::Converged,
            Phase::Blocked,
        ] {
            assert_eq!(Phase::parse(p.as_str()), Some(p));
        }
        assert_eq!(Phase::parse("done"), None);
    }
}
