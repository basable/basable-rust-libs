//! A framework-neutral idempotent provider simulator keyed by a canonical
//! `type/uuid` string, served over HTTP on a loopback port as the Go
//! original was (an `httptest.Server`), so the effect-admission suites can
//! put a proxy between the caller and it. It counts only the FIRST physical
//! creation per key, so a replayed idempotent upsert (the ambiguous-effect
//! case) keeps the count at one — the invariant every effect-idempotency
//! conformance test asserts. `fail_next`, `drop_ack_next` and
//! `set_latency` inject the transient provider conditions the retry and
//! ack-loss scenarios need.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::put;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::Spec;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WidgetResource {
    widgets: i32,
    content: String,
}

#[derive(Default)]
struct SimState {
    resources: HashMap<String, WidgetResource>,
    upserts: HashMap<String, u32>,
    deletes: HashMap<String, u32>,
    fail_next: bool,
    drop_ack_next: bool,
    latency: Duration,
}

/// The simulator: an HTTP server on a loopback port plus a client for it.
pub struct WidgetSim {
    url: String,
    client: reqwest::Client,
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
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .expect("a reqwest client");
        WidgetSim {
            url: format!("http://{addr}"),
            client,
            state,
            server,
        }
    }

    /// The server's base URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The next request fails with 503 before touching state.
    pub fn fail_next(&self) {
        self.lock().fail_next = true;
    }

    /// The next upsert applies, then answers 503: the ack is lost.
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

    /// Creates or replaces the resource under `key` over HTTP.
    pub async fn upsert(&self, key: &str, spec: &Spec) -> Result<(), reqwest::Error> {
        let body = WidgetResource {
            widgets: spec.widgets,
            content: spec.content.clone(),
        };
        self.client
            .put(format!("{}/widgets/{}", self.url, urlencode(key)))
            .json(&body)
            .send()
            .await?
            .error_for_status()
            .map(|_| ())
    }

    /// Deletes the resource under `key` over HTTP.
    pub async fn delete(&self, key: &str) -> Result<(), reqwest::Error> {
        self.client
            .delete(format!("{}/widgets/{}", self.url, urlencode(key)))
            .send()
            .await?
            .error_for_status()
            .map(|_| ())
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

/// The provider key for one object: the type name and the object's UUID.
pub fn canonical_key(type_name: &str, id: uuid::Uuid) -> String {
    format!("{type_name}/{id}")
}

fn urlencode(key: &str) -> String {
    key.replace('/', "%2F")
}

async fn gate(state: &Arc<Mutex<SimState>>) -> Result<(), StatusCode> {
    let (latency, fail) = {
        let mut s = state.lock().unwrap_or_else(|p| p.into_inner());
        let fail = std::mem::take(&mut s.fail_next);
        (s.latency, fail)
    };
    if !latency.is_zero() {
        tokio::time::sleep(latency).await;
    }
    if fail {
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
        assert!(sim.upsert(&key, &spec).await.is_err());
        sim.upsert(&key, &spec).await.unwrap();

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
}
