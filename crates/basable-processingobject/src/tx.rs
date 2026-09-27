//! The restricted transaction handed to adapter callbacks. It runs
//! statements and nothing else: transaction lifecycle belongs to the
//! framework (invariants 2, 3 and 5 are transaction shapes, and an adapter
//! that could commit would break all three). Go documented that as an
//! interface with three methods; here `Tx` wraps the connection and
//! implements only [`Executor`], so `begin`, `commit` and `rollback` are
//! unreachable by type.

use sqlx::{Executor, PgConnection, Postgres};

/// A framework-owned transaction, statements only.
#[derive(Debug)]
pub struct Tx<'c>(pub(crate) &'c mut PgConnection);

impl<'c> Tx<'c> {
    pub(crate) fn new(conn: &'c mut PgConnection) -> Tx<'c> {
        Tx(conn)
    }
}

impl<'t, 'c> Executor<'t> for &'t mut Tx<'c> {
    type Database = Postgres;

    fn fetch_many<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> futures_core::stream::BoxStream<
        'e,
        Result<
            sqlx::Either<
                <Postgres as sqlx::Database>::QueryResult,
                <Postgres as sqlx::Database>::Row,
            >,
            sqlx::Error,
        >,
    >
    where
        't: 'e,
        E: 'q + sqlx::Execute<'q, Postgres>,
    {
        (&mut *self.0).fetch_many(query)
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> futures_core::future::BoxFuture<
        'e,
        Result<Option<<Postgres as sqlx::Database>::Row>, sqlx::Error>,
    >
    where
        't: 'e,
        E: 'q + sqlx::Execute<'q, Postgres>,
    {
        (&mut *self.0).fetch_optional(query)
    }

    fn prepare_with<'e, 'q: 'e>(
        self,
        sql: &'q str,
        parameters: &'e [<Postgres as sqlx::Database>::TypeInfo],
    ) -> futures_core::future::BoxFuture<
        'e,
        Result<<Postgres as sqlx::Database>::Statement<'q>, sqlx::Error>,
    >
    where
        't: 'e,
    {
        (&mut *self.0).prepare_with(sql, parameters)
    }

    fn describe<'e, 'q: 'e>(
        self,
        sql: &'q str,
    ) -> futures_core::future::BoxFuture<'e, Result<sqlx::Describe<Postgres>, sqlx::Error>>
    where
        't: 'e,
    {
        (&mut *self.0).describe(sql)
    }
}
