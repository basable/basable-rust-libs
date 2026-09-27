//! Typed sqlx queries over this nanoservice's own pool (schema
//! `nano_order`). Only this crate queries these tables; another
//! nanoservice that needs the data sends a message (the Directive §2).
//! Errors are `sqlx::Error` as they come; the handler decides what a
//! failure means to its caller (`AppError::wrap`).

use uuid::Uuid;

use crate::model::*;
use crate::Order;

impl Order {
    // --- order_audit ---

    pub async fn insert_order_audit(&self, _row: &OrderAudit) -> Result<(), sqlx::Error> {
        todo!("insert into order_audit: fill the columns in the migration and model.rs first")
    }

    pub async fn get_order_audit(&self, _id: Uuid) -> Result<Option<OrderAudit>, sqlx::Error> {
        todo!("select from order_audit")
    }
}
