//! The two pool types: one per stateful nanoservice, and the migrator.

use std::marker::PhantomData;
use std::ops::Deref;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Connection, Executor, PgConnection, Postgres};

use crate::Stateful;

/// Pool sizing. The defaults suit one nanoservice of a replica against the
/// template's `max_connections = 100`: six stateful nanoservices at eight
/// connections each leave room for Kratos, the migration Job and CNPG's own
/// sessions. `min_connections` is zero so an idle nanoservice costs nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolConfig {
    /// The most connections the pool opens.
    pub max_connections: u32,
    /// Connections kept open when idle.
    pub min_connections: u32,
    /// How long `acquire` waits for a connection before failing.
    pub acquire_timeout: Duration,
    /// How long a connection may live before it is recycled.
    pub max_lifetime: Duration,
    /// How long a connection may sit idle before it is closed.
    pub idle_timeout: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        PoolConfig {
            max_connections: 8,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(10),
            max_lifetime: Duration::from_secs(60 * 60),
            idle_timeout: Duration::from_secs(30 * 60),
        }
    }
}

impl PoolConfig {
    fn apply(self, options: PgPoolOptions) -> PgPoolOptions {
        options
            .max_connections(self.max_connections)
            .min_connections(self.min_connections)
            .acquire_timeout(self.acquire_timeout)
            .max_lifetime(self.max_lifetime)
            .idle_timeout(self.idle_timeout)
    }
}

/// The pool of one stateful nanoservice.
///
/// Every connection is the `app` login switched to the nanoservice's role
/// with its `search_path` pinned, done in `after_connect` so it holds for
/// the connection's whole life: a query that names another nanoservice's
/// table fails with SQLSTATE 42501, and an unqualified name resolves in the
/// nanoservice's own schema only. The pool derefs to [`PgPool`] and `&pool`
/// is an [`Executor`], so sqlx queries take it directly.
///
/// `RESET ROLE` would undo the switch for that connection; nothing in the
/// frameworks issues it, and the clippy aspect has no lint for a SQL
/// string, so this is the one convention the type cannot enforce. Per-role
/// logins would close it and are a `connect` variant away when an
/// environment can mint them.
pub struct NanoPool<N: Stateful> {
    inner: PgPool,
    _nanoservice: PhantomData<fn() -> N>,
}

impl<N: Stateful> Clone for NanoPool<N> {
    fn clone(&self) -> Self {
        NanoPool {
            inner: self.inner.clone(),
            _nanoservice: PhantomData,
        }
    }
}

impl<N: Stateful> std::fmt::Debug for NanoPool<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NanoPool")
            .field("nanoservice", &N::NAME)
            .field("size", &self.inner.size())
            .finish()
    }
}

impl<N: Stateful> NanoPool<N> {
    /// Opens the pool with the `app` login's connect options and proves the
    /// role switch on a first connection, so a missing role or a login that
    /// is not a member of it fails here, at boot, and not on the first query.
    pub async fn connect(
        options: PgConnectOptions,
        config: PoolConfig,
    ) -> Result<Self, sqlx::Error> {
        let role = N::role();
        let schema = N::schema();
        // Prove the switch on one direct connection first: inside the pool
        // the hook's failure only surfaces as an acquire timeout, because the
        // pool retries it. Here a missing role or a login that is not a
        // member fails at boot with the database's own error.
        let mut probe = PgConnection::connect_with(&options).await?;
        switch_role(&mut probe, &role, &schema).await?;
        probe.close().await?;
        let pool = config
            .apply(PgPoolOptions::new())
            .after_connect(move |conn, _meta| {
                let role = role.clone();
                let schema = schema.clone();
                Box::pin(async move { switch_role(conn, &role, &schema).await })
            })
            .connect_with(options)
            .await?;
        tracing::info!(nanoservice = N::NAME, role = %N::role(), "nanoservice pool established");
        Ok(NanoPool {
            inner: pool,
            _nanoservice: PhantomData,
        })
    }

    /// The nanoservice this pool belongs to.
    pub fn nanoservice(&self) -> &'static str {
        N::NAME
    }

    /// The underlying sqlx pool.
    pub fn pool(&self) -> &PgPool {
        &self.inner
    }

    /// Closes the pool, waiting for checked-out connections to return.
    pub async fn close(&self) {
        self.inner.close().await;
    }
}

/// `SET ROLE` is session-scoped and `search_path` set by `ALTER ROLE` applies
/// only at login for the session user, so both are set here explicitly. Two
/// statements rather than one string: `search_path` is an identifier list,
/// and quoting it keeps a nanoservice name from being read as two.
async fn switch_role(conn: &mut PgConnection, role: &str, schema: &str) -> Result<(), sqlx::Error> {
    conn.execute(format!("SET ROLE {}", quote_ident(role)).as_str())
        .await?;
    conn.execute(format!("SET search_path = {}", quote_ident(schema)).as_str())
        .await?;
    Ok(())
}

/// Quotes a SQL identifier. Role and schema names come from a validated
/// nanoservice name, so this is belt and braces, not the safety.
fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

impl<N: Stateful> Deref for NanoPool<N> {
    type Target = PgPool;

    fn deref(&self) -> &PgPool {
        &self.inner
    }
}

impl<'p, N: Stateful> Executor<'p> for &'_ NanoPool<N> {
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
        'p: 'e,
        E: 'q + sqlx::Execute<'q, Postgres>,
    {
        (&self.inner).fetch_many(query)
    }

    fn fetch_optional<'e, 'q: 'e, E>(
        self,
        query: E,
    ) -> futures_core::future::BoxFuture<
        'e,
        Result<Option<<Postgres as sqlx::Database>::Row>, sqlx::Error>,
    >
    where
        'p: 'e,
        E: 'q + sqlx::Execute<'q, Postgres>,
    {
        (&self.inner).fetch_optional(query)
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
        'p: 'e,
    {
        (&self.inner).prepare_with(sql, parameters)
    }

    fn describe<'e, 'q: 'e>(
        self,
        sql: &'q str,
    ) -> futures_core::future::BoxFuture<'e, Result<sqlx::Describe<Postgres>, sqlx::Error>>
    where
        'p: 'e,
    {
        (&self.inner).describe(sql)
    }
}

/// The `app` login as itself: the owner of every schema, which the
/// migrations and the testkit need and nanoservice code must never hold. It
/// is a distinct type for that reason alone; it derefs to [`PgPool`].
#[derive(Clone, Debug)]
pub struct MigratorPool {
    inner: PgPool,
}

impl MigratorPool {
    /// Opens the pool and proves the connection with a round trip.
    pub async fn connect(
        options: PgConnectOptions,
        config: PoolConfig,
    ) -> Result<Self, sqlx::Error> {
        let pool = config
            .apply(PgPoolOptions::new())
            .connect_with(options)
            .await?;
        drop(pool.acquire().await?);
        Ok(MigratorPool { inner: pool })
    }

    /// The underlying sqlx pool.
    pub fn pool(&self) -> &PgPool {
        &self.inner
    }

    /// Closes the pool, waiting for checked-out connections to return.
    pub async fn close(&self) {
        self.inner.close().await;
    }
}

impl Deref for MigratorPool {
    type Target = PgPool;

    fn deref(&self) -> &PgPool {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_quoted() {
        assert_eq!(quote_ident("nano_orders"), "\"nano_orders\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }
}
