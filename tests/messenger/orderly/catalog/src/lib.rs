//! A stub `catalog`: answers product requests and records the events it
//! sees in the shared trace.

use std::sync::{Arc, Mutex};

use basable_app::Component;
use basable_core::{AppError, Ctx};
use interfaces::{CatalogHandler, CatalogRoutes, CatalogSender};
use messages::*;

pub struct Catalog {
    trace: Arc<Mutex<Vec<String>>>,
}

impl Catalog {
    pub fn new(trace: Arc<Mutex<Vec<String>>>) -> Self {
        Catalog { trace }
    }
}

impl<R: CatalogRoutes> CatalogHandler<R> for Catalog {
    async fn handle_upsert_product_request(
        &self,
        _ctx: &Ctx,
        msg: UpsertProductRequest,
        _s: CatalogSender<'_, R>,
    ) -> Result<Product, AppError> {
        self.trace
            .lock()
            .unwrap()
            .push(format!("catalog:upsert:{}", msg.name));
        Ok(Product { name: msg.name })
    }

    async fn handle_get_product_request(
        &self,
        _ctx: &Ctx,
        msg: GetProductRequest,
        _s: CatalogSender<'_, R>,
    ) -> Result<Product, AppError> {
        self.trace
            .lock()
            .unwrap()
            .push(format!("catalog:get:{}", msg.name));
        if msg.name == "missing" {
            return Err(AppError::not_found("no such product"));
        }
        Ok(Product { name: msg.name })
    }

    async fn handle_order_event(&self, _ctx: &Ctx, msg: OrderEvent, _s: CatalogSender<'_, R>) {
        // A yield before recording: the fan-out is sequential, so the
        // second handler must still see this entry first.
        tokio::task::yield_now().await;
        self.trace
            .lock()
            .unwrap()
            .push(format!("catalog:event:{}", msg.order));
    }
}

/// No loops: the default.
impl<R> Component<R> for Catalog {}
