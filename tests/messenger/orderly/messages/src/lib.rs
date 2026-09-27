//! The messages of the orderly topology: plain data, `Clone` because a
//! fan-out clones the message for all but its last handler.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertProductRequest {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetProductRequest {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderEvent {
    pub order: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureOrderRequest {
    pub product: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelOrderRequest {
    pub order: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub id: u64,
    pub product: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendNotificationRequest {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationReceipt {
    pub text: String,
}
