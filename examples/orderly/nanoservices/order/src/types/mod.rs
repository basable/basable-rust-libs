//! One module per processing-object type this nanoservice owns. Each has
//! its own `TypedStore` and its own `Worker`; the framework offers no
//! cross-type write path (the Directive §3 invariant 8).

pub mod order;
pub mod shipment;
