//! The framework list: a keyset page over one type's envelopes, newest
//! first, with the typed rows attached. It is the port of Go's
//! `TypedStore.InspectPage` — the same order (`created_at` then `id`, both
//! descending), the same cursor (the last item's pair), tombstones included
//! unless asked otherwise — widened for an application's own screens: the
//! page carries whole [`Object`]s from one snapshot, not envelope summaries,
//! and a [`ListQuery`] narrows by namespace, labels, phase and deletion
//! intent (`docs/porting-notes.md` 97).
//!
//! It is a read for people and their screens, never a reconcile input: a
//! reconciler sees its object through its claim, and a pass that needs
//! other objects asks their owner over the messenger. A namespace or label
//! filter rides the envelope's indexes; an unfiltered page sorts the type's
//! partition by `created_at`, so its cost grows with the type's size, as
//! Go's does.

use std::fmt;
use std::str::FromStr;

use basable_core::labels::{Labels, validate_labels};
use basable_db::begin_snapshot;
use chrono::{DateTime, Utc};
use sqlx::Postgres;
use sqlx::postgres::PgArguments;
use sqlx::query::Query;
use uuid::Uuid;

use crate::claim::label_selector_sql;
use crate::decl::Adapter;
use crate::error::Error;
use crate::model::{Meta, Object, Phase};
use crate::store::TypedStore;
use crate::store_read::{META_COLUMNS, scan_meta};

/// The page size a [`ListQuery::default`] asks for.
pub const DEFAULT_LIST_LIMIT: u32 = 50;

/// The largest page [`TypedStore::list`] serves; a larger `limit` is
/// [`Error::InvalidConfig`]. Go's `inspect.MaxLimit`.
pub const MAX_LIST_LIMIT: u32 = 200;

/// Which objects a list includes by their deletion intent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Deleting {
    /// Live objects and tombstones alike: Go's `InspectPage`.
    #[default]
    Include,
    /// Live objects only: `deleted_at IS NULL`.
    Exclude,
    /// Objects whose deletion has been requested and not yet carried out.
    Only,
}

/// The position after the last item of a page: its `created_at` and `id`,
/// the keyset the list orders by. A cursor is a value of the page it came
/// from, so an object created or deleted between two pages shifts nothing:
/// the next page starts strictly after the pair, whatever moved around it.
///
/// Its `Display` is an opaque token (`<microseconds since the epoch>.<id>`)
/// for a page token in an API, and `FromStr` parses one back; the token
/// keeps the microsecond `created_at` the column holds, so a round trip is
/// exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cursor {
    /// The last item's `created_at`.
    pub created_at: DateTime<Utc>,
    /// The last item's id.
    pub id: Uuid,
}

impl Cursor {
    /// The cursor after `obj`.
    pub fn after<S, T>(obj: &Object<S, T>) -> Cursor {
        Cursor {
            created_at: obj.meta.created_at,
            id: obj.id,
        }
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}",
            self.created_at.timestamp_micros(),
            self.id.as_simple()
        )
    }
}

/// A page token that is not a [`Cursor`]'s `Display`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidCursor {
    token: String,
}

impl fmt::Display for InvalidCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid list cursor {:?}", self.token)
    }
}

impl std::error::Error for InvalidCursor {}

impl FromStr for Cursor {
    type Err = InvalidCursor;

    fn from_str(token: &str) -> Result<Cursor, InvalidCursor> {
        let invalid = || InvalidCursor {
            token: token.to_string(),
        };
        let (micros, id) = token.split_once('.').ok_or_else(invalid)?;
        let micros: i64 = micros.parse().map_err(|_| invalid())?;
        let created_at = DateTime::from_timestamp_micros(micros).ok_or_else(invalid)?;
        let id = Uuid::try_parse(id).map_err(|_| invalid())?;
        if id.is_nil() {
            return Err(invalid());
        }
        Ok(Cursor { created_at, id })
    }
}

/// What a page holds and where it starts. Every filter is conjunctive and
/// off by default; the default query is the first page of the whole type,
/// newest first, tombstones included, [`DEFAULT_LIST_LIMIT`] long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListQuery {
    /// Only objects whose identity is in this configuration namespace.
    pub namespace: Option<Uuid>,
    /// Only objects whose labels contain every listed pair (JSONB
    /// containment, the claim scan's selector); empty means unfiltered.
    pub labels: Labels,
    /// Only objects in this coarse phase.
    pub phase: Option<Phase>,
    /// Which objects count by their deletion intent.
    pub deleting: Deleting,
    /// Start strictly after this position; `None` is the first page.
    pub after: Option<Cursor>,
    /// The page size: at least one, at most [`MAX_LIST_LIMIT`].
    pub limit: u32,
}

impl Default for ListQuery {
    fn default() -> ListQuery {
        ListQuery {
            namespace: None,
            labels: Labels::new(),
            phase: None,
            deleting: Deleting::Include,
            after: None,
            limit: DEFAULT_LIST_LIMIT,
        }
    }
}

impl ListQuery {
    /// The first page of the whole type, `limit` long.
    pub fn first(limit: u32) -> ListQuery {
        ListQuery {
            limit,
            ..ListQuery::default()
        }
    }

    /// The same query, starting after `cursor`.
    pub fn after(mut self, cursor: Cursor) -> ListQuery {
        self.after = Some(cursor);
        self
    }

    /// The same query, narrowed to `namespace`.
    pub fn in_namespace(mut self, namespace: Uuid) -> ListQuery {
        self.namespace = Some(namespace);
        self
    }

    /// The same query, narrowed to objects whose labels contain `labels`.
    pub fn with_labels(mut self, labels: Labels) -> ListQuery {
        self.labels = labels;
        self
    }

    /// The same query, narrowed to `phase`.
    pub fn in_phase(mut self, phase: Phase) -> ListQuery {
        self.phase = Some(phase);
        self
    }

    /// The same query, with this deletion-intent filter.
    pub fn deleting(mut self, deleting: Deleting) -> ListQuery {
        self.deleting = deleting;
        self
    }

    fn validate(&self) -> Result<(), Error> {
        if self.limit == 0 || self.limit > MAX_LIST_LIMIT {
            return Err(Error::invalid(format!(
                "list limit {} is not within 1..={MAX_LIST_LIMIT}",
                self.limit
            )));
        }
        if let Some(c) = &self.after
            && c.id.is_nil()
        {
            return Err(Error::invalid("list cursor has a nil id"));
        }
        validate_labels(&self.labels).map_err(|e| Error::invalid(format!("list labels: {e}")))?;
        Ok(())
    }
}

/// One page: the objects, newest first, and the cursor of the next page
/// when one exists. `next` is exact — the list looks one row past the page
/// — so the last page never has to be asked for twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<S, T> {
    /// The objects of this page, in list order.
    pub items: Vec<Object<S, T>>,
    /// Where the next page starts, or `None` on the last page.
    pub next: Option<Cursor>,
}

impl<S, T, A> TypedStore<S, T, A>
where
    S: Send + Sync + 'static,
    T: Send + Sync + 'static,
    A: Adapter<S, T>,
{
    /// One page of this type's objects for `q`: newest first by
    /// `(created_at, id)`, from one `REPEATABLE READ READ ONLY` snapshot
    /// with the typed rows attached, and the cursor of the page after it.
    /// A `limit` outside `1..=MAX_LIST_LIMIT` or an invalid label is
    /// [`Error::InvalidConfig`]; an envelope whose typed row is missing is
    /// [`Error::Invariant`].
    pub async fn list(&self, q: &ListQuery) -> Result<Page<S, T>, Error> {
        q.validate()?;
        let op = format!("list {} page", self.inner.decl.name);
        let (sql, binds) = self.list_sql(q)?;
        let mut tx = begin_snapshot(&self.inner.pool)
            .await
            .map_err(|e| Error::sql(format!("{op}: begin"), e))?;
        let rows = binds
            .apply(sqlx::query(&sql))
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| Error::sql(format!("{op}: envelopes"), e))?;
        let mut envelopes: Vec<(Uuid, Meta)> = Vec::with_capacity(rows.len());
        for row in &rows {
            envelopes
                .push(scan_meta(row).map_err(|e| Error::sql(format!("{op}: scan envelope"), e))?);
        }
        // One past the page tells whether a next page exists; it is never
        // returned.
        let more = envelopes.len() > q.limit as usize;
        envelopes.truncate(q.limit as usize);
        let items = self.attach_typed(&mut tx, &op, envelopes).await?;
        let next = if more {
            items.last().map(Cursor::after)
        } else {
            None
        };
        Ok(Page { items, next })
    }

    /// The page statement and its binds, in order. The label predicate is
    /// the claim scan's literal, so it is byte-identical to the selector a
    /// logical worker claims by and rides the same GIN index.
    fn list_sql(&self, q: &ListQuery) -> Result<(String, Binds), Error> {
        let mut binds = Binds::default();
        let mut sql = format!(
            "SELECT {META_COLUMNS} FROM {}\n WHERE TRUE\n",
            self.inner.partition
        );
        if let Some(ns) = q.namespace {
            let n = binds.push(Bind::Uuid(ns));
            sql.push_str(&format!("  AND namespace = ${n}\n"));
        }
        if !q.labels.is_empty() {
            sql.push_str(&label_selector_sql(&q.labels)?);
        }
        if let Some(phase) = q.phase {
            let n = binds.push(Bind::Text(phase.as_str()));
            sql.push_str(&format!("  AND phase = ${n}\n"));
        }
        match q.deleting {
            Deleting::Include => {}
            Deleting::Exclude => sql.push_str("  AND deleted_at IS NULL\n"),
            Deleting::Only => sql.push_str("  AND deleted_at IS NOT NULL\n"),
        }
        if let Some(c) = q.after {
            let at = binds.push(Bind::Timestamp(c.created_at));
            let id = binds.push(Bind::Uuid(c.id));
            sql.push_str(&format!(
                "  AND (created_at, id) < (${at}::timestamptz, ${id}::uuid)\n"
            ));
        }
        let limit = binds.push(Bind::Int(i64::from(q.limit) + 1));
        sql.push_str(&format!(
            " ORDER BY created_at DESC, id DESC\n LIMIT ${limit}"
        ));
        Ok((sql, binds))
    }
}

/// A bind parameter of the page statement.
enum Bind {
    Uuid(Uuid),
    Text(&'static str),
    Timestamp(DateTime<Utc>),
    Int(i64),
}

/// The page statement's binds, numbered as they are pushed.
#[derive(Default)]
struct Binds(Vec<Bind>);

impl Binds {
    /// Adds a bind and returns its 1-based placeholder number.
    fn push(&mut self, b: Bind) -> usize {
        self.0.push(b);
        self.0.len()
    }

    fn apply<'q>(
        &'q self,
        mut query: Query<'q, Postgres, PgArguments>,
    ) -> Query<'q, Postgres, PgArguments> {
        for b in &self.0 {
            query = match b {
                Bind::Uuid(v) => query.bind(*v),
                Bind::Text(v) => query.bind(*v),
                Bind::Timestamp(v) => query.bind(*v),
                Bind::Int(v) => query.bind(*v),
            };
        }
        query
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cursor_token_round_trips_at_microsecond_precision() {
        let c = Cursor {
            created_at: DateTime::from_timestamp_micros(1_760_000_000_123_456).unwrap(),
            id: Uuid::new_v4(),
        };
        let token = c.to_string();
        assert_eq!(token, format!("1760000000123456.{}", c.id.as_simple()));
        assert_eq!(token.parse::<Cursor>().unwrap(), c);
    }

    #[test]
    fn a_malformed_token_is_refused() {
        for token in [
            "",
            "123",
            "abc.00000000000000000000000000000001",
            "123.not-a-uuid",
            "123.00000000000000000000000000000000",
            "99999999999999999999.00000000000000000000000000000001",
        ] {
            let err = token.parse::<Cursor>().unwrap_err();
            assert_eq!(
                err,
                InvalidCursor {
                    token: token.to_string()
                },
                "{token:?}"
            );
            assert!(err.to_string().contains(token));
        }
    }

    #[test]
    fn the_limit_is_bounded_and_the_labels_are_checked() {
        assert!(ListQuery::first(0).validate().is_err());
        assert!(ListQuery::first(MAX_LIST_LIMIT + 1).validate().is_err());
        assert!(ListQuery::first(MAX_LIST_LIMIT).validate().is_ok());
        assert!(ListQuery::default().validate().is_ok());
        let bad = ListQuery::default().with_labels(
            [("bad'key".to_string(), "v".to_string())]
                .into_iter()
                .collect(),
        );
        assert!(matches!(bad.validate(), Err(Error::InvalidConfig(_))));
    }
}
