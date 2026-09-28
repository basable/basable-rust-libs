//! A framework-neutral provider simulator served over HTTP on a loopback
//! port as the Go original was (an `httptest.Server`), so the
//! effect-admission suites can put a proxy between the caller and it. Two
//! resources: idempotent widgets keyed by a canonical `type/uuid` string,
//! counting only the FIRST physical creation per key so a replayed
//! idempotent upsert (the ambiguous-effect case) keeps the count at one;
//! and orders, an irreversible resource minted under an idempotency key
//! the receiver replays and re-reads by id or by key. `fail_next`,
//! `reject_next`, `drop_ack_next` and `set_latency` inject the provider
//! conditions the retry, rejection and ack-loss scenarios need.
//!
//! [`WidgetClient`] is the provider client: it maps the transport's errors
//! the way a production provider does at its boundary — a 4xx is a
//! provider-proven rejection (`definitive`), a 5xx a plain error, anything
//! without a status a transport failure (`TransportError`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use basable_core::BoxError;
use basable_externaleffect::{TransportError, definitive};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::Spec;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WidgetResource {
    widgets: i32,
    content: String,
}

/// An order the simulator minted under an idempotency key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Order {
    /// The provider identity.
    pub id: String,
    /// The idempotency key it was minted under.
    pub key: String,
    /// The payload.
    pub widgets: i32,
}

#[derive(Debug, Deserialize, Serialize)]
struct OrderRequest {
    widgets: i32,
}

#[derive(Default)]
struct SimState {
    resources: HashMap<String, WidgetResource>,
    upserts: HashMap<String, u32>,
    deletes: HashMap<String, u32>,
    orders: HashMap<String, Order>,
    order_keys: HashMap<String, String>,
    next_order: u64,
    fail_next: bool,
    reject_next: bool,
    drop_ack_next: bool,
    latency: Duration,
}

/// The simulator: an HTTP server on a loopback port plus a direct client.
pub struct WidgetSim {
    client: WidgetClient,
    state: Arc<Mutex<SimState>>,
    server: JoinHandle<()>,
}

impl WidgetSim {
    /// Starts the server on a free loopback port.
    pub async fn start() -> WidgetSim {
        let state = Arc::new(Mutex::new(SimState::default()));
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind a loopback port for the widget simulator");
        let addr = listener.local_addr().expect("the listener's address");
        let app = Router::new()
            .route("/widgets/{key}", put(handle_put).delete(handle_delete))
            .route("/orders", post(handle_place_order))
            .route("/orders/{id}", get(handle_get_order))
            .route("/orders/by-key/{key}", get(handle_find_order))
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        WidgetSim {
            client: WidgetClient::new(&format!("http://{addr}")),
            state,
            server,
        }
    }

    /// The server's base URL.
    pub fn url(&self) -> &str {
        &self.client.url
    }

    /// The direct client.
    pub fn client(&self) -> WidgetClient {
        self.client.clone()
    }

    /// A client reaching the server through `base_url` — a proxy in front
    /// of it.
    pub fn client_via(&self, base_url: &str) -> WidgetClient {
        WidgetClient::new(base_url)
    }

    /// The next request fails with 503 before touching state.
    pub fn fail_next(&self) {
        self.lock().fail_next = true;
    }

    /// The next request is rejected with a structured 400 before touching
    /// state — the provider's proof of non-execution.
    pub fn reject_next(&self) {
        self.lock().reject_next = true;
    }

    /// The next write applies, then answers 503: the ack is lost.
    pub fn drop_ack_next(&self) {
        self.lock().drop_ack_next = true;
    }

    /// Every request waits this long first.
    pub fn set_latency(&self, d: Duration) {
        self.lock().latency = d;
    }

    /// How many times the resource under `key` was physically created.
    pub fn count(&self, key: &str) -> u32 {
        self.lock().upserts.get(key).copied().unwrap_or(0)
    }

    /// How many times a resource under `key` was deleted.
    pub fn delete_count(&self, key: &str) -> u32 {
        self.lock().deletes.get(key).copied().unwrap_or(0)
    }

    /// Whether a resource exists under `key`.
    pub fn exists(&self, key: &str) -> bool {
        self.lock().resources.contains_key(key)
    }

    /// Every key with a resource.
    pub fn keys(&self) -> Vec<String> {
        self.lock().resources.keys().cloned().collect()
    }

    /// How many orders exist — the receiver's ground truth for the
    /// irreversible effect.
    pub fn order_count(&self) -> usize {
        self.lock().orders.len()
    }

    /// Creates or replaces the resource under `key` over HTTP.
    pub async fn upsert(&self, key: &str, spec: &Spec) -> Result<(), BoxError> {
        self.client.upsert(key, spec).await
    }

    /// Deletes the resource under `key` over HTTP.
    pub async fn delete(&self, key: &str) -> Result<(), BoxError> {
        self.client.delete(key).await
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SimState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Drop for WidgetSim {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// The provider client for a [`WidgetSim`], direct or through a proxy.
#[derive(Debug, Clone)]
pub struct WidgetClient {
    url: String,
    client: reqwest::Client,
}

impl WidgetClient {
    /// A client for the simulator at `base_url`. Connections are not
    /// pooled: every request is its own connection, which is what lets a
    /// proxy in front of the server act per request.
    pub fn new(base_url: &str) -> WidgetClient {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .pool_max_idle_per_host(0)
            .build()
            .expect("a reqwest client");
        WidgetClient {
            url: base_url.trim_end_matches('/').to_owned(),
            client,
        }
    }

    /// The base URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Creates or replaces the widget resource under `key`.
    pub async fn upsert(&self, key: &str, spec: &Spec) -> Result<(), BoxError> {
        let body = WidgetResource {
            widgets: spec.widgets,
            content: spec.content.clone(),
        };
        self.client
            .put(format!("{}/widgets/{}", self.url, urlencode(key)))
            .json(&body)
            .send()
            .await
            .map_err(map_err)?
            .error_for_status()
            .map(|_| ())
            .map_err(map_err)
    }

    /// Deletes the widget resource under `key`.
    pub async fn delete(&self, key: &str) -> Result<(), BoxError> {
        self.client
            .delete(format!("{}/widgets/{}", self.url, urlencode(key)))
            .send()
            .await
            .map_err(map_err)?
            .error_for_status()
            .map(|_| ())
            .map_err(map_err)
    }

    /// Places an order under an idempotency key: the receiver mints one
    /// order per key and replays it for the same key.
    pub async fn place_order(
        &self,
        idempotency_key: &str,
        widgets: i32,
    ) -> Result<Order, BoxError> {
        let resp = self
            .client
            .post(format!("{}/orders", self.url))
            .header("idempotency-key", idempotency_key)
            .json(&OrderRequest { widgets })
            .send()
            .await
            .map_err(map_err)?
            .error_for_status()
            .map_err(map_err)?;
        resp.json().await.map_err(map_err)
    }

    /// Reads an order by its provider identity; an unknown id is a
    /// provider-proven rejection.
    pub async fn get_order(&self, id: &str) -> Result<Order, BoxError> {
        let resp = self
            .client
            .get(format!("{}/orders/{}", self.url, urlencode(id)))
            .send()
            .await
            .map_err(map_err)?
            .error_for_status()
            .map_err(map_err)?;
        resp.json().await.map_err(map_err)
    }

    /// Reads the order minted under `key`, `None` when verifiably absent.
    pub async fn find_order_by_key(&self, key: &str) -> Result<Option<Order>, BoxError> {
        let resp = self
            .client
            .get(format!("{}/orders/by-key/{}", self.url, urlencode(key)))
            .send()
            .await
            .map_err(map_err)?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = resp.error_for_status().map_err(map_err)?;
        resp.json().await.map(Some).map_err(map_err)
    }
}

/// The provider boundary's error mapping: a 4xx proves the receiver
/// refused before acting; a 5xx is the receiver's plain answer; anything
/// without a status never got one — a transport failure.
fn map_err(e: reqwest::Error) -> BoxError {
    match e.status() {
        Some(s) if s.is_client_error() => definitive(e),
        Some(_) => Box::new(e),
        None => TransportError::wrap(e),
    }
}

/// The provider key for one object: the type name and the object's UUID.
pub fn canonical_key(type_name: &str, id: uuid::Uuid) -> String {
    format!("{type_name}/{id}")
}

fn urlencode(key: &str) -> String {
    key.replace('/', "%2F")
}

async fn gate(state: &Arc<Mutex<SimState>>) -> Result<(), StatusCode> {
    let (latency, fail, reject) = {
        let mut s = state.lock().unwrap_or_else(|p| p.into_inner());
        let fail = std::mem::take(&mut s.fail_next);
        let reject = std::mem::take(&mut s.reject_next);
        (s.latency, fail, reject)
    };
    if !latency.is_zero() {
        tokio::time::sleep(latency).await;
    }
    if reject {
        Err(StatusCode::BAD_REQUEST)
    } else if fail {
        Err(StatusCode::SERVICE_UNAVAILABLE)
    } else {
        Ok(())
    }
}

async fn handle_put(
    State(state): State<Arc<Mutex<SimState>>>,
    Path(key): Path<String>,
    Json(resource): Json<WidgetResource>,
) -> StatusCode {
    if let Err(code) = gate(&state).await {
        return code;
    }
    let mut s = state.lock().unwrap_or_else(|p| p.into_inner());
    // The side-effect counter counts only the first physical creation for a
    // key; replayed idempotent upserts preserve a count of one.
    if !s.resources.contains_key(&key) {
        *s.upserts.entry(key.clone()).or_insert(0) += 1;
    }
    s.resources.insert(key, resource);
    if std::mem::take(&mut s.drop_ack_next) {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::NO_CONTENT
}

async fn handle_delete(
    State(state): State<Arc<Mutex<SimState>>>,
    Path(key): Path<String>,
) -> StatusCode {
    if let Err(code) = gate(&state).await {
        return code;
    }
    let mut s = state.lock().unwrap_or_else(|p| p.into_inner());
    if s.resources.remove(&key).is_some() {
        *s.deletes.entry(key).or_insert(0) += 1;
    }
    StatusCode::NO_CONTENT
}

async fn handle_place_order(
    State(state): State<Arc<Mutex<SimState>>>,
    headers: HeaderMap,
    Json(req): Json<OrderRequest>,
) -> Result<(StatusCode, Json<Order>), StatusCode> {
    gate(&state).await?;
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|k| !k.is_empty())
        .ok_or(StatusCode::BAD_REQUEST)?
        .to_owned();
    let mut s = state.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(id) = s.order_keys.get(&key).cloned() {
        let order = s.orders[&id].clone();
        return Ok((StatusCode::OK, Json(order)));
    }
    s.next_order += 1;
    let order = Order {
        id: format!("ord_{}", s.next_order),
        key: key.clone(),
        widgets: req.widgets,
    };
    s.orders.insert(order.id.clone(), order.clone());
    s.order_keys.insert(key, order.id.clone());
    if std::mem::take(&mut s.drop_ack_next) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok((StatusCode::CREATED, Json(order)))
}

async fn handle_get_order(
    State(state): State<Arc<Mutex<SimState>>>,
    Path(id): Path<String>,
) -> Result<Json<Order>, StatusCode> {
    gate(&state).await?;
    let s = state.lock().unwrap_or_else(|p| p.into_inner());
    s.orders
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn handle_find_order(
    State(state): State<Arc<Mutex<SimState>>>,
    Path(key): Path<String>,
) -> Result<Json<Order>, StatusCode> {
    gate(&state).await?;
    let s = state.lock().unwrap_or_else(|p| p.into_inner());
    s.order_keys
        .get(&key)
        .and_then(|id| s.orders.get(id))
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn upserts_are_counted_once_and_faults_fire_once() {
        let sim = WidgetSim::start().await;
        let key = canonical_key("conformance", uuid::Uuid::new_v4());
        let spec = Spec {
            widgets: 2,
            content: "c".into(),
        };

        sim.upsert(&key, &spec).await.unwrap();
        sim.upsert(&key, &spec).await.unwrap();
        assert_eq!(
            sim.count(&key),
            1,
            "a replayed idempotent upsert is not a second creation"
        );
        assert!(sim.exists(&key));
        assert_eq!(sim.keys(), vec![key.clone()]);

        sim.fail_next();
        let err = sim.upsert(&key, &spec).await.unwrap_err();
        assert!(
            !basable_externaleffect::is_definitive(&*err),
            "a 503 is the receiver's plain answer, not its proof"
        );
        sim.upsert(&key, &spec).await.unwrap();

        sim.reject_next();
        let err = sim.upsert(&key, &spec).await.unwrap_err();
        assert!(
            basable_externaleffect::is_definitive(&*err),
            "a 400 is proof"
        );

        sim.drop_ack_next();
        let other = canonical_key("conformance", uuid::Uuid::new_v4());
        assert!(
            sim.upsert(&other, &spec).await.is_err(),
            "applied, ack lost"
        );
        assert!(sim.exists(&other));
        assert_eq!(sim.count(&other), 1);

        sim.delete(&key).await.unwrap();
        assert!(!sim.exists(&key));
        assert_eq!(sim.delete_count(&key), 1);
        sim.delete(&key).await.unwrap();
        assert_eq!(
            sim.delete_count(&key),
            1,
            "deleting an absent resource counts nothing"
        );
    }

    #[tokio::test]
    async fn orders_replay_by_key_and_read_back() {
        let sim = WidgetSim::start().await;
        let client = sim.client();
        let first = client.place_order("k1", 3).await.unwrap();
        let replay = client.place_order("k1", 3).await.unwrap();
        assert_eq!(first, replay, "the same key replays the same order");
        assert_eq!(sim.order_count(), 1);
        let second = client.place_order("k2", 1).await.unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(sim.order_count(), 2);

        assert_eq!(client.get_order(&first.id).await.unwrap(), first);
        let missing = client.get_order("ord_none").await.unwrap_err();
        assert!(
            basable_externaleffect::is_definitive(&*missing),
            "an unknown id is a 404: proof"
        );
        assert_eq!(client.find_order_by_key("k2").await.unwrap(), Some(second));
        assert_eq!(client.find_order_by_key("k9").await.unwrap(), None);

        let unreachable = WidgetClient::new("http://127.0.0.1:1");
        let err = unreachable.place_order("k", 1).await.unwrap_err();
        assert!(
            basable_externaleffect::is_transport(&*err),
            "a refused connection is a transport failure: {err}"
        );
    }
}
