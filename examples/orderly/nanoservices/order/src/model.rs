//! Row structs of this nanoservice's plain tables, one per table, mirroring
//! the columns in db/app/migrations exactly.

use serde::{Deserialize, Serialize};

/// A row of `nano_order.order_audit`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct OrderAudit {
    pub id: uuid::Uuid,
    // TODO: the columns declared in the migration.
}

