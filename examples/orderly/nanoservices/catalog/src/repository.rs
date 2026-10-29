//! Typed sqlx queries over this nanoservice's own pool (schema
//! `nano_catalog`). Only this crate queries these tables; another
//! nanoservice that needs the data sends a message (the Directive §2).
//! Errors are `sqlx::Error` as they come; the handler decides what a
//! failure means to its caller (`AppError::wrap`).

use uuid::Uuid;

// Rows are `model::<Table>`, qualified: the component struct below and a
// message type in `messages` may carry the same name as a table.
use crate::model;
use crate::Catalog;

impl Catalog {
    // --- product ---

    pub async fn insert_product(&self, row: &model::Product) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO product (id, sku, price_cents) VALUES ($1, $2, $3)",
        )
        .bind(row.id)
        .bind(&row.sku)
        .bind(row.price_cents)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_product(&self, id: Uuid) -> Result<Option<model::Product>, sqlx::Error> {
        sqlx::query_as::<_, model::Product>("SELECT id, sku, price_cents FROM product WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }
}
