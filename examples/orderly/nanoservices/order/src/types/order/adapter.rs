//! The adapter: a dumb column mapper between `Spec`/`Status` and the
//! `order_spec` / `order_status` tables. Five callbacks
//! plus the optional finalize, every one database-only over the restricted
//! `Tx`; every column present in BOTH `write_spec` and `read_rows`, so a
//! column can never be silently lost. Never touches envelope columns.
//! Errors are `sqlx::Error`: a column mapper fails no other way, and the
//! framework wraps them.

use basable_processingobject::{Adapter as AdapterTrait, Object, Ref, Row, Tx};
use uuid::Uuid;

use super::{Spec, Status};

pub struct Adapter;

impl AdapterTrait<Spec, Status> for Adapter {
    async fn insert_spec(&self, tx: &mut Tx<'_>, r: &Ref, spec: &Spec) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO order_spec (id, customer_id, lines) VALUES ($1, $2, $3)")
            .bind(r.id)
            .bind(spec.customer_id)
            .bind(&spec.lines)
            .execute(tx)
            .await?;
        Ok(())
    }

    async fn insert_status(&self, tx: &mut Tx<'_>, r: &Ref, status: &Status) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO order_status (id, phase, payment_id) VALUES ($1, $2, $3)")
            .bind(r.id)
            .bind(&status.phase)
            .bind(&status.payment_id)
            .execute(tx)
            .await?;
        Ok(())
    }

    async fn read_rows(&self, tx: &mut Tx<'_>, ids: &[Uuid]) -> Result<Vec<Row<Spec, Status>>, sqlx::Error> {
        // Batched by contract: one query for the whole claim batch.
        let rows = sqlx::query(
            "SELECT s.id, s.customer_id, s.lines, t.phase, t.payment_id \
             FROM order_spec s JOIN order_status t ON t.id = s.id WHERE s.id = ANY($1)",
        )
        .bind(ids)
        .fetch_all(tx)
        .await?;
        rows.into_iter()
            .map(|row| {
                use sqlx::Row as _;
                Ok(Row {
                    id: row.try_get("id")?,
                    spec: Spec {
                        customer_id: row.try_get("customer_id")?,
                        lines: row.try_get("lines")?,
                    },
                    status: Status {
                        phase: row.try_get("phase")?,
                        payment_id: row.try_get("payment_id")?,
                    },
                })
            })
            .collect()
    }

    async fn write_spec(&self, tx: &mut Tx<'_>, r: &Ref, spec: &Spec) -> Result<(), sqlx::Error> {
        // Immutable columns are omitted: the migration's trigger guards them.
        sqlx::query("UPDATE order_spec SET updated_at = clock_timestamp(), lines = $2 WHERE id = $1")
            .bind(r.id)
            .bind(&spec.lines)
            .execute(tx)
            .await?;
        Ok(())
    }

    async fn write_status(&self, tx: &mut Tx<'_>, r: &Ref, status: &Status) -> Result<(), sqlx::Error> {
        // The whole row, unconditionally: single provenance (invariant 3).
        sqlx::query("UPDATE order_status SET updated_at = clock_timestamp(), phase = $2, payment_id = $3 WHERE id = $1")
            .bind(r.id)
            .bind(&status.phase)
            .bind(&status.payment_id)
            .execute(tx)
            .await?;
        Ok(())
    }

    async fn finalize_delete(&self, _tx: &mut Tx<'_>, _obj: &Object<Spec, Status>) -> Result<(), sqlx::Error> {
        // Optional teardown evidence, run atomically with the envelope DELETE.
        Ok(())
    }
}
