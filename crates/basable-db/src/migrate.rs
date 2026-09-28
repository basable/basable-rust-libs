//! The dbmate-format migration runner and ledger check.
//!
//! Production migrations run in the dbmate Job the scaffold ships, from one
//! flat `db/app/migrations/` directory. This runner reads the same files,
//! writes the same `schema_migrations` ledger, and exists for what dbmate
//! cannot be in the loop for: the testkit's per-test databases, local
//! bootstrap, and the boot-time check that the ledger holds every version
//! the binary expects ([`verify`]).
//!
//! The format: `<version>_<name>.sql`, a `-- migrate:up` block and an
//! optional `-- migrate:down` block, either marker optionally followed by
//! `transaction:false`. Files apply in version order, each inside one
//! transaction unless it opts out, so a `SET LOCAL ROLE` at the top of a
//! nanoservice's file scopes to that file. Versions already in the ledger
//! are skipped; the ledger row is written in the same transaction as the
//! migration.

use std::fmt;
use std::path::{Path, PathBuf};

use sqlx::{Acquire, Executor, PgConnection, PgPool};

/// One parsed migration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// The version: the file name up to the first underscore, dbmate's key.
    pub version: String,
    /// The rest of the file name, for messages.
    pub name: String,
    /// The `-- migrate:up` block.
    pub up: String,
    /// The `-- migrate:down` block, empty when absent.
    pub down: String,
    /// Whether the up block runs inside a transaction (`transaction:false`
    /// on the marker opts out, for statements Postgres refuses in one).
    pub transaction: bool,
}

/// Why migrations could not be read, applied or verified.
#[derive(Debug)]
pub enum MigrateError {
    /// A directory or file could not be read.
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The error.
        source: std::io::Error,
    },
    /// A file is not a dbmate migration.
    Malformed {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// Two files carry the same version.
    DuplicateVersion(String),
    /// A statement failed; the version's transaction rolled back.
    Sql {
        /// The migration that failed.
        version: String,
        /// The error.
        source: sqlx::Error,
    },
    /// The ledger itself could not be read or written.
    Ledger(sqlx::Error),
    /// [`verify`] found versions the ledger lacks.
    Missing(Vec<String>),
}

impl fmt::Display for MigrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MigrateError::Io { path, source } => {
                write!(f, "migrations: reading {}: {source}", path.display())
            }
            MigrateError::Malformed { path, reason } => {
                write!(
                    f,
                    "migrations: {} is not a dbmate migration: {reason}",
                    path.display()
                )
            }
            MigrateError::DuplicateVersion(v) => {
                write!(f, "migrations: two files carry version {v}")
            }
            MigrateError::Sql { version, source } => {
                write!(f, "migration {version} failed: {source}")
            }
            MigrateError::Ledger(e) => write!(f, "migrations: schema_migrations ledger: {e}"),
            MigrateError::Missing(v) => {
                write!(
                    f,
                    "migrations: the database lacks version(s) {}",
                    v.join(", ")
                )
            }
        }
    }
}

impl std::error::Error for MigrateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MigrateError::Io { source, .. } => Some(source),
            MigrateError::Sql { source, .. } | MigrateError::Ledger(source) => Some(source),
            MigrateError::Malformed { .. }
            | MigrateError::DuplicateVersion(_)
            | MigrateError::Missing(_) => None,
        }
    }
}

const UP_MARKER: &str = "-- migrate:up";
const DOWN_MARKER: &str = "-- migrate:down";

/// Parses one file's text. `path` is for the error and the version.
pub fn parse(path: &Path, text: &str) -> Result<Migration, MigrateError> {
    let malformed = |reason| MigrateError::Malformed {
        path: path.to_path_buf(),
        reason,
    };
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".sql"))
        .ok_or_else(|| malformed("the file name is not <version>_<name>.sql"))?;
    let (version, name) = stem
        .split_once('_')
        .filter(|(v, n)| !v.is_empty() && !n.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
        .ok_or_else(|| malformed("the file name is not <digits>_<name>.sql"))?;

    let up_at = text
        .find(UP_MARKER)
        .ok_or_else(|| malformed("no `-- migrate:up` marker"))?;
    let down_at = text.find(DOWN_MARKER);
    if down_at.is_some_and(|d| d < up_at) {
        return Err(malformed("`-- migrate:down` comes before `-- migrate:up`"));
    }
    let (up_line, up_body) = split_marker(&text[up_at..], UP_MARKER, down_at.map(|d| d - up_at));
    let down = match down_at {
        Some(d) => split_marker(&text[d..], DOWN_MARKER, None).1,
        None => String::new(),
    };
    Ok(Migration {
        version: version.to_owned(),
        name: name.to_owned(),
        up: up_body,
        down,
        transaction: !up_line.contains("transaction:false"),
    })
}

/// Splits a marker line from its block; `end` bounds the block when another
/// marker follows.
fn split_marker(text: &str, marker: &str, end: Option<usize>) -> (String, String) {
    let block = &text[..end.unwrap_or(text.len())];
    let (line, body) = block.split_once('\n').unwrap_or((block, ""));
    (line[marker.len()..].to_owned(), body.trim().to_owned())
}

/// Reads every `*.sql` file of a directory, sorted by version.
pub fn read_dir(dir: &Path) -> Result<Vec<Migration>, MigrateError> {
    let io = |source| MigrateError::Io {
        path: dir.to_path_buf(),
        source,
    };
    let mut migrations = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let path = entry.map_err(io)?.path();
        if path.extension().is_some_and(|e| e == "sql") {
            let text = std::fs::read_to_string(&path).map_err(|source| MigrateError::Io {
                path: path.clone(),
                source,
            })?;
            migrations.push(parse(&path, &text)?);
        }
    }
    migrations.sort_by(|a, b| a.version.cmp(&b.version));
    for pair in migrations.windows(2) {
        if pair[0].version == pair[1].version {
            return Err(MigrateError::DuplicateVersion(pair[0].version.clone()));
        }
    }
    Ok(migrations)
}

/// The ledger dbmate creates: dbmate's own table name and column width, so
/// the Job and this runner read and write the same rows.
const CREATE_LEDGER: &str =
    "CREATE TABLE IF NOT EXISTS public.schema_migrations (version VARCHAR(128) PRIMARY KEY)";

/// Applies every migration whose version the ledger lacks, in order, and
/// returns the versions applied. `replacements` rewrites the SQL text
/// before it runs, the test harness's way of pointing a hard-coded name at a
/// per-test object (the Go harness replaced `DATABASE config` this way).
pub async fn apply(
    pool: &PgPool,
    migrations: &[Migration],
    replacements: &[(&str, &str)],
) -> Result<Vec<String>, MigrateError> {
    let mut conn = pool.acquire().await.map_err(MigrateError::Ledger)?;
    conn.execute(CREATE_LEDGER)
        .await
        .map_err(MigrateError::Ledger)?;
    let applied = applied_versions_on(&mut conn).await?;
    let mut done = Vec::new();
    for m in migrations {
        if applied.contains(&m.version) {
            continue;
        }
        let mut sql = m.up.clone();
        for (from, to) in replacements {
            sql = sql.replace(from, to);
        }
        let failed = |source| MigrateError::Sql {
            version: m.version.clone(),
            source,
        };
        if m.transaction {
            let mut tx = conn.begin().await.map_err(failed)?;
            sqlx::raw_sql(&sql)
                .execute(&mut *tx)
                .await
                .map_err(failed)?;
            record(&mut tx, &m.version).await.map_err(failed)?;
            tx.commit().await.map_err(failed)?;
        } else {
            sqlx::raw_sql(&sql)
                .execute(&mut *conn)
                .await
                .map_err(failed)?;
            record(&mut conn, &m.version).await.map_err(failed)?;
        }
        tracing::info!(version = %m.version, name = %m.name, "migration applied");
        done.push(m.version.clone());
    }
    Ok(done)
}

async fn record(conn: &mut PgConnection, version: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO public.schema_migrations (version) VALUES ($1)")
        .bind(version)
        .execute(conn)
        .await?;
    Ok(())
}

/// The versions the ledger holds, sorted. An absent ledger is an empty one.
pub async fn applied_versions(pool: &PgPool) -> Result<Vec<String>, MigrateError> {
    let mut conn = pool.acquire().await.map_err(MigrateError::Ledger)?;
    applied_versions_on(&mut conn).await
}

async fn applied_versions_on(conn: &mut PgConnection) -> Result<Vec<String>, MigrateError> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT version FROM public.schema_migrations ORDER BY version")
            .fetch_all(&mut *conn)
            .await
            .or_else(|e| {
                // No ledger yet: nothing applied. Anything else is the caller's.
                if crate::sqlstate::of(&e) == Some("42P01") {
                    Ok(Vec::new())
                } else {
                    Err(e)
                }
            })
            .map_err(MigrateError::Ledger)?;
    Ok(rows.into_iter().map(|(v,)| v).collect())
}

/// Checks the ledger holds every `expected` version, the boot-time gate: a
/// binary that embeds a migration the Job has not applied refuses to serve
/// (exit 3 in the template's `main`) instead of failing on the first query
/// that needs the missing column.
pub async fn verify(pool: &PgPool, expected: &[&str]) -> Result<(), MigrateError> {
    let applied = applied_versions(pool).await?;
    let missing: Vec<String> = expected
        .iter()
        .filter(|v| !applied.iter().any(|a| a == *v))
        .map(|v| (*v).to_owned())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(MigrateError::Missing(missing))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_up_and_down_blocks() {
        let m = parse(
            Path::new("db/app/migrations/20260101000100_orders_init.sql"),
            "-- migrate:up\nCREATE TABLE a (id int);\n\n-- migrate:down\nDROP TABLE a;\n",
        )
        .unwrap();
        assert_eq!(m.version, "20260101000100");
        assert_eq!(m.name, "orders_init");
        assert_eq!(m.up, "CREATE TABLE a (id int);");
        assert_eq!(m.down, "DROP TABLE a;");
        assert!(m.transaction);
    }

    #[test]
    fn a_missing_down_block_and_a_transaction_opt_out() {
        let m = parse(
            Path::new("1_x.sql"),
            "-- migrate:up transaction:false\nCREATE INDEX CONCURRENTLY i ON a (id);",
        )
        .unwrap();
        assert_eq!(m.down, "");
        assert!(!m.transaction);
        assert_eq!(m.up, "CREATE INDEX CONCURRENTLY i ON a (id);");
    }

    #[test]
    fn refuses_what_dbmate_would() {
        for (name, text) in [
            ("nounderscore.sql", "-- migrate:up\nselect 1;"),
            ("abc_x.sql", "-- migrate:up\nselect 1;"),
            ("1_x.sql", "select 1;"),
            (
                "1_x.sql",
                "-- migrate:down\nselect 1;\n-- migrate:up\nselect 2;",
            ),
            ("1_x.txt", "-- migrate:up\nselect 1;"),
        ] {
            assert!(
                matches!(
                    parse(Path::new(name), text),
                    Err(MigrateError::Malformed { .. })
                ),
                "{name}"
            );
        }
    }
}
