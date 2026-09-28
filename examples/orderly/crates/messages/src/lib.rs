//! The messages nanoservices exchange. Every `message:` named in
//! `routing.yaml` is a struct here — the messenger codegen references them by
//! name, so a missing one is a build error naming it.
//!
//! Rules: a message carries the SENDER's generation when it hands derived
//! state to another nanoservice (the Directive §7); it names things by the
//! caller's seam (a name, never an id the receiver mints); only derived
//! state crosses. Keep them plain data: no methods that reach a database.

#![allow(unused_imports)]

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// basable:messages-begin — declare one struct per message in routing.yaml.

/// `UpsertProductRequest` — answered with `Product` (see routing.yaml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertProductRequest {
    // TODO: the fields.
}

/// `GetProductRequest` — answered with `Product` (see routing.yaml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetProductRequest {
    // TODO: the fields.
}

/// `OrderEvent` — a void fan-out event (see routing.yaml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderEvent {
    // TODO: the fields.
}

/// `EnsureOrderRequest` — answered with `Order` (see routing.yaml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsureOrderRequest {
    // TODO: the fields.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Product {
    // TODO: the fields.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    // TODO: the fields.
}
// basable:messages-end
