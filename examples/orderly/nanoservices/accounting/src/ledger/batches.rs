//! The batches of this ledger (the Directive §11), rendered from
//! `owns.ledger.batches` in the manifest. One value of [`Batch`] is one batch:
//! the variant is the kind, which has a table of its own, and the fields say
//! which one. A desired state names batches by value and reads nothing; the
//! engine finds each one's row with [`PgBatch::find`], and the commit that
//! first books on a batch creates its row with [`PgBatch::ensure`], in one
//! transaction with the journal.
//!
//! One sequence, `batch_key_seq`, allocates `batch_key` over every table here,
//! because a stored line carries that key alone: the same number in two tables
//! would be one batch. A row is for good: its primary key, `batch_key`,
//! `ledger` and `batch_type` never change and it is never deleted. A batch is
//! closed when a booking brings its balance to zero; nothing marks it.
//!
//! Adding a kind later: the manifest entry, a new dbmate migration creating
//! its table with `batch_key BIGINT NOT NULL UNIQUE DEFAULT
//! nextval('batch_key_seq')`, and an arm in each `match` below, in the same
//! form. Never `ON CONFLICT DO UPDATE`: it would lock the row until commit,
//! and every booking on a hot batch would queue.

use basable_ledger::{BatchType, LedgerBatch, LedgerKey, StoredBatch};
use basable_ledger_postgres::{PgBatch, stored_batch};
use sqlx::PgConnection;

/// One batch. Each variant carries its table's key columns and the other
/// columns a new row must have; a nullable column is filled later.
#[derive(Clone, PartialEq, Debug)]
pub enum Batch {
    /// A `debit` batch in `receivable_batch`, ledger 1.
    Receivable {
        /// The `order_id` column.
        order_id: uuid::Uuid,
    },
    /// A `debit` batch in `cash_batch`, ledger 1.
    Cash {
        /// The `month` column.
        month: String,
    },
    /// A `credit` batch in `revenue_batch`, ledger 1.
    Revenue {
        /// The `month` column.
        month: String,
    },
}

/// What identifies a batch: its table's key columns and nothing else. Two
/// values with one primary key are one batch, whatever else they carry.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum BatchPrimaryKey {
    /// The key of a `receivable` batch.
    Receivable {
        /// The `order_id` column.
        order_id: uuid::Uuid,
    },
    /// The key of a `cash` batch.
    Cash {
        /// The `month` column.
        month: String,
    },
    /// The key of a `revenue` batch.
    Revenue {
        /// The `month` column.
        month: String,
    },
}

impl LedgerBatch for Batch {
    type PrimaryKey = BatchPrimaryKey;

    fn primary_key(&self) -> BatchPrimaryKey {
        match self {
            Batch::Receivable { order_id, .. } => BatchPrimaryKey::Receivable {
                order_id: *order_id,
            },
            Batch::Cash { month, .. } => BatchPrimaryKey::Cash {
                month: month.clone(),
            },
            Batch::Revenue { month, .. } => BatchPrimaryKey::Revenue {
                month: month.clone(),
            },
        }
    }

    fn ledger(&self) -> LedgerKey {
        match self {
            Batch::Receivable { .. } => LedgerKey(1),
            Batch::Cash { .. } => LedgerKey(1),
            Batch::Revenue { .. } => LedgerKey(1),
        }
    }

    fn batch_type(&self) -> BatchType {
        match self {
            Batch::Receivable { .. } => BatchType::Debit,
            Batch::Cash { .. } => BatchType::Debit,
            Batch::Revenue { .. } => BatchType::Credit,
        }
    }
}

impl PgBatch for Batch {
    async fn find(&self, conn: &mut PgConnection) -> Result<Option<StoredBatch>, sqlx::Error> {
        let row = match self {
            Batch::Receivable { order_id, .. } => {
                sqlx::query("SELECT batch_key, ledger, batch_type FROM receivable_batch WHERE order_id = $1")
                    .bind(order_id)
                    .fetch_optional(&mut *conn)
                    .await?
            }
            Batch::Cash { month, .. } => {
                sqlx::query("SELECT batch_key, ledger, batch_type FROM cash_batch WHERE month = $1")
                    .bind(month)
                    .fetch_optional(&mut *conn)
                    .await?
            }
            Batch::Revenue { month, .. } => {
                sqlx::query("SELECT batch_key, ledger, batch_type FROM revenue_batch WHERE month = $1")
                    .bind(month)
                    .fetch_optional(&mut *conn)
                    .await?
            }
        };
        row.as_ref().map(stored_batch).transpose()
    }

    async fn ensure(&self, tx: &mut PgConnection) -> Result<StoredBatch, sqlx::Error> {
        let ledger = self.ledger().0 as i32;
        let batch_type = self.batch_type().as_i16();
        match self {
            Batch::Receivable { order_id, } => {
                sqlx::query("INSERT INTO receivable_batch (order_id, ledger, batch_type) VALUES ($1, $2, $3) ON CONFLICT (order_id) DO NOTHING")
                    .bind(order_id)
                    .bind(ledger)
                    .bind(batch_type)
                    .execute(&mut *tx)
                    .await?;
            }
            Batch::Cash { month, } => {
                sqlx::query("INSERT INTO cash_batch (month, ledger, batch_type) VALUES ($1, $2, $3) ON CONFLICT (month) DO NOTHING")
                    .bind(month)
                    .bind(ledger)
                    .bind(batch_type)
                    .execute(&mut *tx)
                    .await?;
            }
            Batch::Revenue { month, } => {
                sqlx::query("INSERT INTO revenue_batch (month, ledger, batch_type) VALUES ($1, $2, $3) ON CONFLICT (month) DO NOTHING")
                    .bind(month)
                    .bind(ledger)
                    .bind(batch_type)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        self.find(tx).await?.ok_or(sqlx::Error::RowNotFound)
    }
}
