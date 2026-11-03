//! The suites against a real database, skipped without `TEST_DATABASE_URL`.
//! They live inside the crate because the fixture's `Batch` is the testkit's
//! and `PgBatch` is ours: only this crate may join the two. The batch tables
//! stand in for the ones a scaffolded project renders: several tables, one
//! sequence.

mod postgres;
mod restatement;

use basable_db::{Nanoservice, Stateful};
use basable_ledger::{Ledger, StoredBatch};
use basable_ledger_testkit::fixture::{
    Batch, Invoice, InvoiceLedger, Payment, PaymentLedger, Transfer, TransferLedger, VatReturn,
    VatReturnLedger,
};
use basable_testkit::{TestDb, runfile};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{PgBatch, PgStore, SCHEMA_PLACEHOLDER, migrations, stored_batch};

pub(crate) struct Orders;
impl Nanoservice for Orders {
    const NAME: &'static str = "orders";
}
impl Stateful for Orders {}

const BATCH_TABLES: &str = "
    CREATE SEQUENCE batch_key_seq;
    CREATE TABLE object_batch (
        kind TEXT NOT NULL,
        object_id UUID NOT NULL,
        batch_key BIGINT NOT NULL UNIQUE DEFAULT nextval('batch_key_seq'),
        ledger INTEGER NOT NULL,
        batch_type SMALLINT NOT NULL,
        PRIMARY KEY (kind, object_id)
    );
    CREATE TABLE period_batch (
        kind TEXT NOT NULL,
        period TEXT NOT NULL,
        batch_key BIGINT NOT NULL UNIQUE DEFAULT nextval('batch_key_seq'),
        ledger INTEGER NOT NULL,
        batch_type SMALLINT NOT NULL,
        PRIMARY KEY (kind, period)
    );
    CREATE TABLE settlement_batch (
        merchant TEXT PRIMARY KEY,
        settle_day TEXT NOT NULL,
        batch_key BIGINT NOT NULL UNIQUE DEFAULT nextval('batch_key_seq'),
        ledger INTEGER NOT NULL,
        batch_type SMALLINT NOT NULL
    );
    CREATE TABLE named_batch (
        name TEXT PRIMARY KEY,
        batch_key BIGINT NOT NULL UNIQUE DEFAULT nextval('batch_key_seq'),
        ledger INTEGER NOT NULL,
        batch_type SMALLINT NOT NULL
    );
    CREATE TABLE in_book_batch (
        name TEXT NOT NULL,
        ledger INTEGER NOT NULL,
        batch_key BIGINT NOT NULL UNIQUE DEFAULT nextval('batch_key_seq'),
        batch_type SMALLINT NOT NULL,
        PRIMARY KEY (name, ledger)
    );
";

const COLUMNS: &str = "batch_key, ledger, batch_type";

/// The table and key of the kinds that share a table by kind.
enum Keyed<'a> {
    Object(&'a str, Uuid),
    Period(&'a str, &'a str),
}

fn keyed(batch: &Batch) -> Option<Keyed<'_>> {
    Some(match batch {
        Batch::Receivable(id) => Keyed::Object("receivable", id.0),
        Batch::CashClearing(id) => Keyed::Object("cash", id.0),
        Batch::Revenue(p) => Keyed::Period("revenue", p),
        Batch::Vat(p) => Keyed::Period("vat", p),
        Batch::TaxOffice(p) => Keyed::Period("tax_office", p),
        Batch::BadDebt(p) => Keyed::Period("bad_debt", p),
        Batch::Points(p) => Keyed::Period("points", p),
        Batch::Settlement { .. } | Batch::Named { .. } | Batch::InBook { .. } => return None,
    })
}

impl PgBatch for Batch {
    async fn find(&self, conn: &mut PgConnection) -> Result<Option<StoredBatch>, sqlx::Error> {
        use basable_ledger::LedgerBatch;
        let row = match (keyed(self), self) {
            (Some(Keyed::Object(kind, id)), _) => {
                sqlx::query(&format!(
                    "SELECT {COLUMNS} FROM object_batch WHERE kind = $1 AND object_id = $2"
                ))
                .bind(kind)
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?
            }
            (Some(Keyed::Period(kind, period)), _) => {
                sqlx::query(&format!(
                    "SELECT {COLUMNS} FROM period_batch WHERE kind = $1 AND period = $2"
                ))
                .bind(kind)
                .bind(period)
                .fetch_optional(&mut *conn)
                .await?
            }
            (None, Batch::Settlement { merchant, .. }) => {
                sqlx::query(&format!(
                    "SELECT {COLUMNS} FROM settlement_batch WHERE merchant = $1"
                ))
                .bind(merchant)
                .fetch_optional(&mut *conn)
                .await?
            }
            (None, Batch::Named { name, .. }) => {
                sqlx::query(&format!(
                    "SELECT {COLUMNS} FROM named_batch WHERE name = $1"
                ))
                .bind(name)
                .fetch_optional(&mut *conn)
                .await?
            }
            (None, Batch::InBook { name, .. }) => {
                sqlx::query(&format!(
                    "SELECT {COLUMNS} FROM in_book_batch WHERE name = $1 AND ledger = $2"
                ))
                .bind(name)
                .bind(self.ledger().0 as i32)
                .fetch_optional(&mut *conn)
                .await?
            }
            (None, _) => unreachable!("every other kind is keyed"),
        };
        row.as_ref().map(stored_batch).transpose()
    }

    async fn ensure(&self, tx: &mut PgConnection) -> Result<StoredBatch, sqlx::Error> {
        use basable_ledger::LedgerBatch;
        let ledger = self.ledger().0 as i32;
        let batch_type = self.batch_type().as_i16();
        match (keyed(self), self) {
            (Some(Keyed::Object(kind, id)), _) => {
                sqlx::query(
                    "INSERT INTO object_batch (kind, object_id, ledger, batch_type) \
                     VALUES ($1, $2, $3, $4) ON CONFLICT (kind, object_id) DO NOTHING",
                )
                .bind(kind)
                .bind(id)
                .bind(ledger)
                .bind(batch_type)
                .execute(&mut *tx)
                .await?;
            }
            (Some(Keyed::Period(kind, period)), _) => {
                sqlx::query(
                    "INSERT INTO period_batch (kind, period, ledger, batch_type) \
                     VALUES ($1, $2, $3, $4) ON CONFLICT (kind, period) DO NOTHING",
                )
                .bind(kind)
                .bind(period)
                .bind(ledger)
                .bind(batch_type)
                .execute(&mut *tx)
                .await?;
            }
            (
                None,
                Batch::Settlement {
                    merchant,
                    settle_day,
                },
            ) => {
                sqlx::query(
                    "INSERT INTO settlement_batch (merchant, settle_day, ledger, batch_type) \
                     VALUES ($1, $2, $3, $4) ON CONFLICT (merchant) DO NOTHING",
                )
                .bind(merchant)
                .bind(settle_day)
                .bind(ledger)
                .bind(batch_type)
                .execute(&mut *tx)
                .await?;
            }
            (None, Batch::Named { name, .. }) => {
                sqlx::query(
                    "INSERT INTO named_batch (name, ledger, batch_type) \
                     VALUES ($1, $2, $3) ON CONFLICT (name) DO NOTHING",
                )
                .bind(name)
                .bind(ledger)
                .bind(batch_type)
                .execute(&mut *tx)
                .await?;
            }
            (None, Batch::InBook { name, .. }) => {
                sqlx::query(
                    "INSERT INTO in_book_batch (name, ledger, batch_type) \
                     VALUES ($1, $2, $3) ON CONFLICT (name, ledger) DO NOTHING",
                )
                .bind(name)
                .bind(ledger)
                .bind(batch_type)
                .execute(&mut *tx)
                .await?;
            }
            (None, _) => unreachable!("every other kind is keyed"),
        }
        self.find(tx).await?.ok_or(sqlx::Error::RowNotFound)
    }
}

/// The fixture database with the ledger's and the batch tables in the orders
/// schema, or `None` without `TEST_DATABASE_URL`.
pub(crate) async fn db() -> Option<TestDb> {
    let db = TestDb::from_env_with_migrations(&runfile(
        "crates/basable-testkit/tests/fixtures/migrations",
    ))
    .await?;
    let ms = migrations().expect("migration parses");
    basable_db::migrate::apply(db.migrator(), &ms, &[(SCHEMA_PLACEHOLDER, "nano_orders")])
        .await
        .expect("ledger tables applied");
    let pool = db.nano_pool::<Orders>().await;
    sqlx::raw_sql(BATCH_TABLES)
        .execute(pool.pool())
        .await
        .expect("batch tables created");
    pool.close().await;
    Some(db)
}

pub(crate) async fn store(db: &TestDb) -> PgStore<Batch> {
    let pool = db.nano_pool::<Orders>().await;
    PgStore::new(pool.pool().clone())
}

pub(crate) fn ledger(store: PgStore<Batch>) -> Ledger<PgStore<Batch>> {
    Ledger::builder(store)
        .register::<Invoice, InvoiceLedger>()
        .register::<Payment, PaymentLedger>()
        .register::<VatReturn, VatReturnLedger>()
        .register::<Transfer, TransferLedger>()
        .build()
}
