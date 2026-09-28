//! Row structs of this nanoservice's plain tables, one per table, mirroring
//! the columns in db/app/migrations exactly.

use serde::{Deserialize, Serialize};

/// A row of `nano_catalog.product`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Product {
    pub id: uuid::Uuid,
    pub sku: String,
    pub price_cents: i64,
}

