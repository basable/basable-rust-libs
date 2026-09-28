//! A database per test with the migrations applied.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use basable_db::migrate::{self, MigrateError};
use basable_db::{MigratorPool, NanoPool, PoolConfig, Stateful};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Executor, PgPool};

/// The variable naming the Postgres the tests may use: a login that can
/// create roles and databases (the `postgres` image's `POSTGRES_USER`).
pub const DATABASE_URL_VAR: &str = "TEST_DATABASE_URL";
/// The variable naming the migrations directory, when it is not the
/// tenant's `db/app/migrations` in the test's runfiles.
pub const MIGRATIONS_DIR_VAR: &str = "TEST_MIGRATIONS_DIR";

/// The login CNPG creates for the application, which the migrations name
/// in their GRANTs and which owns every schema. The harness creates it once
/// per server; the password is fixed because the server is a test container.
const APP_LOGIN: &str = "app";
const APP_PASSWORD: &str = "app";

/// A database created for one test.
pub struct TestDb {
    admin: PgPool,
    admin_options: PgConnectOptions,
    name: String,
    migrator: MigratorPool,
    superuser: PgPool,
}

/// Why a test database could not be set up.
#[derive(Debug)]
pub enum TestDbError {
    /// `TEST_DATABASE_URL` is not a Postgres URL.
    Url(sqlx::Error),
    /// The admin login could not create the role or the database.
    Admin(sqlx::Error),
    /// The `app` login could not connect to the new database.
    Connect(sqlx::Error),
    /// The migrations could not be read or applied.
    Migrate(MigrateError),
}

impl fmt::Display for TestDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestDbError::Url(e) => write!(f, "{DATABASE_URL_VAR} is not a Postgres URL: {e}"),
            TestDbError::Admin(e) => write!(f, "creating the test role or database: {e}"),
            TestDbError::Connect(e) => {
                write!(f, "connecting as {APP_LOGIN} to the test database: {e}")
            }
            TestDbError::Migrate(e) => write!(f, "applying the migrations: {e}"),
        }
    }
}

impl std::error::Error for TestDbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TestDbError::Url(e) | TestDbError::Admin(e) | TestDbError::Connect(e) => Some(e),
            TestDbError::Migrate(e) => Some(e),
        }
    }
}

impl TestDb {
    /// A test database with the tenant's migrations (`db/app/migrations` in
    /// the runfiles, or [`MIGRATIONS_DIR_VAR`]) applied, or `None` when
    /// [`DATABASE_URL_VAR`] is unset. A setup failure panics: it is a test.
    pub async fn from_env() -> Option<TestDb> {
        let dir = std::env::var_os(MIGRATIONS_DIR_VAR)
            .map(PathBuf::from)
            .unwrap_or_else(|| runfile("db/app/migrations"));
        TestDb::from_env_with_migrations(&dir).await
    }

    /// [`TestDb::from_env`] with an explicit migrations directory.
    pub async fn from_env_with_migrations(migrations: &Path) -> Option<TestDb> {
        let Ok(url) = std::env::var(DATABASE_URL_VAR) else {
            eprintln!("{DATABASE_URL_VAR} is not set; skipping the database-backed test");
            return None;
        };
        match TestDb::create(&url, migrations).await {
            Ok(db) => Some(db),
            Err(e) => panic!("test database: {e}"),
        }
    }

    /// Creates the database, the `app` login if the server lacks it, and
    /// applies `migrations` as that login.
    pub async fn create(url: &str, migrations: &Path) -> Result<TestDb, TestDbError> {
        let admin_options = PgConnectOptions::from_str(url).map_err(TestDbError::Url)?;
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(admin_options.clone())
            .await
            .map_err(TestDbError::Admin)?;
        ensure_app_login(&admin).await.map_err(TestDbError::Admin)?;

        let name = database_name();
        admin
            .execute(format!("CREATE DATABASE {name} OWNER {APP_LOGIN}").as_str())
            .await
            .map_err(TestDbError::Admin)?;

        let app_options = admin_options
            .clone()
            .username(APP_LOGIN)
            .password(APP_PASSWORD)
            .database(&name);
        let migrator = MigratorPool::connect(app_options, PoolConfig::default())
            .await
            .map_err(TestDbError::Connect)?;
        let files = migrate::read_dir(migrations).map_err(TestDbError::Migrate)?;
        migrate::apply(&migrator, &files, &[])
            .await
            .map_err(TestDbError::Migrate)?;
        let superuser = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(admin_options.clone().database(&name))
            .await
            .map_err(TestDbError::Admin)?;

        Ok(TestDb {
            admin,
            admin_options,
            name,
            migrator,
            superuser,
        })
    }

    /// The database's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The `app` login on this database: the owner of every schema, as the
    /// migration Job and local bootstrap hold it. Never what a nanoservice
    /// holds.
    pub fn migrator(&self) -> &PgPool {
        &self.migrator
    }

    /// The migrator as its own type.
    pub fn migrator_pool(&self) -> &MigratorPool {
        &self.migrator
    }

    /// The server's own login, the one [`DATABASE_URL_VAR`] names, on this
    /// database. It sees every schema regardless of ownership, which is what
    /// an assertion that crosses a nanoservice boundary needs; production
    /// has no such login, so nothing but a test may hold it.
    pub fn superuser(&self) -> &PgPool {
        &self.superuser
    }

    /// Connect options for the `app` login on this database, for a pool the
    /// test builds itself (through a [`crate::CommitFaultProxy`], say).
    pub fn app_options(&self) -> PgConnectOptions {
        self.admin_options
            .clone()
            .username(APP_LOGIN)
            .password(APP_PASSWORD)
            .database(&self.name)
    }

    /// The pool nanoservice `N` would hold in production: the `app` login
    /// switched to `nano_<name>` with its `search_path` pinned.
    pub async fn nano_pool<N: Stateful>(&self) -> NanoPool<N> {
        NanoPool::connect(self.app_options(), PoolConfig::default())
            .await
            .unwrap_or_else(|e| panic!("pool for nanoservice {}: {e}", N::NAME))
    }

    /// Drops the database. A test that fails before calling this leaves its
    /// database for inspection, as the Go harness does on failure.
    pub async fn finish(self) {
        self.migrator.close().await;
        self.superuser.close().await;
        if let Err(e) = self
            .admin
            .execute(format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name).as_str())
            .await
        {
            eprintln!("dropping test database {}: {e}", self.name);
        }
        self.admin.close().await;
    }
}

/// Creates the `app` login once per server. Two tests may race here; the
/// loser sees the catalog's unique violation (not `duplicate_object`, which
/// only a serialised `CREATE ROLE` raises), and both are swallowed inside
/// the block. A login that already exists is left as it is.
async fn ensure_app_login(admin: &PgPool) -> Result<(), sqlx::Error> {
    admin
        .execute(
            format!(
                "DO $$ BEGIN
                    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{APP_LOGIN}') THEN
                        CREATE ROLE {APP_LOGIN} LOGIN CREATEROLE PASSWORD '{APP_PASSWORD}';
                    END IF;
                EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL;
                END $$"
            )
            .as_str(),
        )
        .await?;
    Ok(())
}

/// `test_<test name>_<8 hex>`, lowercased to `[a-z0-9_]` and cut to
/// Postgres's 63 characters; the test name comes from the thread the harness
/// runs it on, as in Go.
fn database_name() -> String {
    let thread = std::thread::current();
    let label = thread.name().unwrap_or("test");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[suffix.len() - 8..];
    let mut name: String = format!("test_{label}")
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '_' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '_',
        })
        .collect();
    name.truncate(63 - 1 - suffix.len());
    format!("{name}_{suffix}")
}

/// A path under the test's runfiles (`$TEST_SRCDIR/$TEST_WORKSPACE/<rel>`)
/// when running under Bazel, else `rel` relative to the crate's manifest
/// directory or the working directory. `data = [...]` on the test target
/// is what puts the files there.
pub fn runfile(rel: &str) -> PathBuf {
    if let (Ok(srcdir), Ok(workspace)) = (
        std::env::var("TEST_SRCDIR"),
        std::env::var("TEST_WORKSPACE"),
    ) {
        return Path::new(&srcdir).join(workspace).join(rel);
    }
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        return Path::new(&manifest).join(rel);
    }
    PathBuf::from(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_names_are_valid_identifiers() {
        let name = database_name();
        assert!(name.starts_with("test_"));
        assert!(name.len() <= 63);
        assert!(
            name.chars()
                .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_')),
            "{name}"
        );
    }
}
