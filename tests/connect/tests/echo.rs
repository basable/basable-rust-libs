//! The Phase 9 verification over the generated EchoService: a Connect
//! client in both codecs over HTTP/1.1 and h2c, plain JSON over reqwest,
//! error codes round-tripping, a server stream, and the auth layer against
//! a Kratos simulator: a valid cookie reaches the handler as its identity,
//! a missing, rejected or inactive session is 401 with no leak, a public
//! path bypasses entirely, the hook fires once per validation, and an
//! unreachable Kratos is 503, never a silent allow.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use basable_app::{App, Config, Identity, Validator};
use basable_auth::AuthCtx;
use basable_connect::{ConnectRouter, IntoConnect, request_ctx};
use basable_core::{AppError, Code};
use basable_testkit::TestDb;
use connectrpc::client::{ClientConfig, HttpClient};
use connectrpc::{
    ErrorCode, RequestContext, Response, ServiceRequest, ServiceResult, ServiceStream,
};
use proto::connect::basable::connecttest::v1::{EchoService, EchoServiceClient};
use proto::proto::basable::connecttest::v1::{
    CountRequest, CountResponse, EchoRequest, EchoResponse, FailRequest, WhoAmIRequest,
    WhoAmIResponse,
};

/// The service under test. The generated trait returns `impl Encodable`;
/// answering with the owned message is the refinement the lint names, and
/// what a tenant's handlers do.
struct Echo;

#[allow(refining_impl_trait)]
impl EchoService for Echo {
    async fn echo(
        &self,
        ctx: RequestContext,
        request: ServiceRequest<'_, EchoRequest>,
    ) -> ServiceResult<EchoResponse> {
        let c = request_ctx(&ctx);
        Ok(Response::new(EchoResponse {
            text: request.to_owned_message().text,
            request_id: c.request_id().to_string(),
            ..Default::default()
        }))
    }

    async fn fail(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, FailRequest>,
    ) -> ServiceResult<EchoResponse> {
        let req = request.to_owned_message();
        let code = Code::from_name(&req.code).unwrap_or(Code::Unknown);
        let r: Result<EchoResponse, AppError> = Err(AppError::new(code, req.message));
        r.into_connect().map(Response::new)
    }

    async fn who_am_i(
        &self,
        ctx: RequestContext,
        _request: ServiceRequest<'_, WhoAmIRequest>,
    ) -> ServiceResult<WhoAmIResponse> {
        let c = request_ctx(&ctx);
        Ok(Response::new(WhoAmIResponse {
            user_id: c.user_id().unwrap_or("").to_string(),
            ..Default::default()
        }))
    }

    async fn count(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, CountRequest>,
    ) -> ServiceResult<ServiceStream<CountResponse>> {
        let upto = request.to_owned_message().upto;
        let items = (1..=upto).map(|n| {
            Ok(CountResponse {
                n,
                ..Default::default()
            })
        });
        Response::stream_ok(futures::stream::iter(items))
    }
}

/// A Kratos simulator: `GET /sessions/whoami` by cookie.
struct Kratos {
    addr: std::net::SocketAddr,
    calls: Arc<AtomicU32>,
    down: Arc<AtomicBool>,
}

async fn kratos() -> Kratos {
    use axum::extract::State;
    use axum::http::{StatusCode, header};
    use axum::routing::get;
    let calls = Arc::new(AtomicU32::new(0));
    let down = Arc::new(AtomicBool::new(false));
    let state = (calls.clone(), down.clone());
    let app = axum::Router::new().route(
        "/sessions/whoami",
        get(
            |State((calls, down)): State<(Arc<AtomicU32>, Arc<AtomicBool>)>, headers: axum::http::HeaderMap| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                if down.load(Ordering::SeqCst) {
                    return (StatusCode::INTERNAL_SERVER_ERROR, [(header::CONTENT_TYPE, "text/plain")], "boom".to_string());
                }
                let cookie = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()).unwrap_or("");
                let (status, body) = if cookie.contains("ory_kratos_session=good") {
                    (StatusCode::OK, r#"{"id":"s-1","active":true,"identity":{"id":"user-1","traits":{"email":"u@example.com"}}}"#)
                } else if cookie.contains("ory_kratos_session=inactive") {
                    (StatusCode::OK, r#"{"id":"s-2","active":false,"identity":{"id":"user-2","traits":{"email":"i@example.com"}}}"#)
                } else {
                    (StatusCode::UNAUTHORIZED, r#"{"error":{"code":401,"message":"no session"}}"#)
                };
                (status, [(header::CONTENT_TYPE, "application/json")], body.to_string())
            },
        ),
    )
    .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Kratos { addr, calls, down }
}

struct Stack {
    running: basable_app::Running,
    kratos: Kratos,
    hook_calls: Arc<AtomicU32>,
    db: TestDb,
}

async fn stack() -> Option<Stack> {
    let db = TestDb::from_env_with_migrations(&basable_testkit::runfile(
        "crates/basable-testkit/tests/fixtures/migrations",
    ))
    .await?;
    let kratos = kratos().await;
    let mut cfg = Config::new("postgres://unused");
    cfg.host = "127.0.0.1".parse().unwrap();
    cfg.port = 0;
    cfg.shutdown_grace_secs = 5;
    let app = App::new(cfg)
        .connect_options(db.app_options())
        .connect()
        .await
        .unwrap();
    let hook_calls = Arc::new(AtomicU32::new(0));
    let h = hook_calls.clone();
    let validator = Validator::kratos(&format!("http://{}", kratos.addr))
        .public_prefixes(["/public/"])
        .session_hook(move |identity| {
            assert_eq!(identity.id, "user-1");
            h.fetch_add(1, Ordering::SeqCst);
        });
    let mut connect = ConnectRouter::new();
    connect.add_service(Arc::new(Echo));
    let raw = axum::Router::new().route("/public/ping", axum::routing::get(|| async { "pong" }));
    let running = app
        .serve()
        .connect(connect)
        .raw(raw)
        .auth(validator)
        .start()
        .await
        .unwrap();
    Some(Stack {
        running,
        kratos,
        hook_calls,
        db,
    })
}

impl Stack {
    fn base(&self) -> String {
        format!("http://{}", self.running.addr())
    }

    fn client(
        &self,
        transport: HttpClient,
        json: bool,
        cookie: Option<&str>,
    ) -> EchoServiceClient<HttpClient> {
        let mut config = ClientConfig::new(self.base().parse().unwrap());
        config = if json { config.json() } else { config.proto() };
        if let Some(c) = cookie {
            config = config.with_default_header("cookie", c);
        }
        EchoServiceClient::new(transport, config)
    }

    async fn finish(self) {
        self.running.shutdown().await.unwrap();
        self.db.finish().await;
    }
}

const GOOD: &str = "ory_kratos_session=good";

#[tokio::test]
async fn the_generated_client_round_trips_in_both_codecs_over_h1_and_h2c() {
    let Some(s) = stack().await else { return };
    for (name, transport, json) in [
        ("proto/h1", HttpClient::plaintext(), false),
        ("json/h1", HttpClient::plaintext(), true),
        ("proto/h2c", HttpClient::plaintext_http2_only(), false),
        ("json/h2c", HttpClient::plaintext_http2_only(), true),
    ] {
        let client = s.client(transport, json, Some(GOOD));
        let resp = client
            .echo(EchoRequest {
                text: format!("hello {name}"),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .into_owned();
        assert_eq!(resp.text, format!("hello {name}"));
        assert!(
            !resp.request_id.is_empty(),
            "{name}: the handler saw a request id"
        );

        let me = client
            .who_am_i(WhoAmIRequest::default())
            .await
            .unwrap()
            .into_owned();
        assert_eq!(me.user_id, "user-1", "{name}");

        let err = client
            .fail(FailRequest {
                code: "not_found".into(),
                message: "no such thing".into(),
                ..Default::default()
            })
            .await
            .map(|r| r.into_owned())
            .expect_err("the failure crosses the wire");
        assert_eq!(err.code, ErrorCode::NotFound, "{name}");
        assert_eq!(err.message.as_deref(), Some("no such thing"), "{name}");
    }
    assert!(
        s.hook_calls.load(Ordering::SeqCst) >= 12,
        "the hook fired per validation"
    );
    s.finish().await;
}

#[tokio::test]
async fn a_server_stream_arrives_in_order() {
    let Some(s) = stack().await else { return };
    let client = s.client(HttpClient::plaintext(), false, Some(GOOD));
    let mut stream = client
        .count(CountRequest {
            upto: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut seen = Vec::new();
    while let Some(item) = stream.message::<CountResponse>().await.unwrap() {
        seen.push(item.to_owned_message().n);
    }
    assert_eq!(seen, vec![1, 2, 3, 4, 5]);
    s.finish().await;
}

#[tokio::test]
async fn plain_json_over_http1_sees_the_connect_error_shape_and_the_request_id() {
    let Some(s) = stack().await else { return };
    let http = reqwest::Client::new();
    let base = s.base();
    let r = http
        .post(format!("{base}/basable.connecttest.v1.EchoService/Echo"))
        .header("content-type", "application/json")
        .header("cookie", GOOD)
        .header("x-request-id", "req-42")
        .body(r#"{"text":"plain"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers().get("x-request-id").unwrap(), "req-42");
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["text"], "plain");
    assert_eq!(body["requestId"], "req-42");

    let r = http
        .post(format!("{base}/basable.connecttest.v1.EchoService/Fail"))
        .header("content-type", "application/json")
        .header("cookie", GOOD)
        .body(r#"{"code":"failed_precondition","message":"not yet"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        400,
        "failed_precondition maps to 400 in Connect"
    );
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["code"], "failed_precondition");
    assert_eq!(body["message"], "not yet");
    s.finish().await;
}

#[tokio::test]
async fn the_auth_layer_rejects_and_bypasses_as_the_go_middleware_did() {
    let Some(s) = stack().await else { return };
    let http = reqwest::Client::new();
    let base = s.base();
    let echo = |cookie: Option<&str>| {
        let mut req = http
            .post(format!("{base}/basable.connecttest.v1.EchoService/Echo"))
            .header("content-type", "application/json")
            .body(r#"{"text":"x"}"#);
        if let Some(c) = cookie {
            req = req.header("cookie", c);
        }
        req.send()
    };

    // No cookie: 401, and Kratos was not asked.
    let before = s.kratos.calls.load(Ordering::SeqCst);
    let r = echo(None).await.unwrap();
    assert_eq!(r.status(), 401);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["code"], "unauthenticated");
    assert_eq!(s.kratos.calls.load(Ordering::SeqCst), before);

    // A rejected cookie: 401, no detail leaks; the second try within the
    // negative window does not reach Kratos again.
    let r = echo(Some("ory_kratos_session=bad")).await.unwrap();
    assert_eq!(r.status(), 401);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["message"], "invalid session");
    let after_first = s.kratos.calls.load(Ordering::SeqCst);
    let r = echo(Some("ory_kratos_session=bad")).await.unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(s.kratos.calls.load(Ordering::SeqCst), after_first);

    // An inactive session: 401.
    let r = echo(Some("ory_kratos_session=inactive")).await.unwrap();
    assert_eq!(r.status(), 401);

    // A public prefix and the probes bypass entirely.
    let r = http
        .get(format!("{base}/public/ping"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.text().await.unwrap(), "pong");
    let r = http.get(format!("{base}/readyz")).send().await.unwrap();
    assert_eq!(r.status(), 200);

    // Kratos down: 503 unavailable, never a silent allow.
    s.kratos.down.store(true, Ordering::SeqCst);
    let r = echo(Some(GOOD)).await.unwrap();
    assert_eq!(r.status(), 503);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["code"], "unavailable");
    s.kratos.down.store(false, Ordering::SeqCst);

    // The good cookie: the handler sees the identity; the hook fired once
    // per validation.
    let hooks = s.hook_calls.load(Ordering::SeqCst);
    let r = http
        .post(format!("{base}/basable.connecttest.v1.EchoService/WhoAmI"))
        .header("content-type", "application/json")
        .header("cookie", GOOD)
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["userId"], "user-1");
    assert_eq!(s.hook_calls.load(Ordering::SeqCst), hooks + 1);
    s.finish().await;
}

#[tokio::test]
async fn the_test_bypass_is_a_fixed_identity() {
    let Some(db) = TestDb::from_env_with_migrations(&basable_testkit::runfile(
        "crates/basable-testkit/tests/fixtures/migrations",
    ))
    .await
    else {
        return;
    };
    let mut cfg = Config::new("postgres://unused");
    cfg.host = "127.0.0.1".parse().unwrap();
    cfg.port = 0;
    let app = App::new(cfg)
        .connect_options(db.app_options())
        .connect()
        .await
        .unwrap();
    let mut connect = ConnectRouter::new();
    connect.add_service(Arc::new(Echo));
    let validator = Validator::bypassed_for_tests(Identity {
        id: "test-user".into(),
        email: "t@example.com".into(),
    });
    let running = app
        .serve()
        .connect(connect)
        .auth(validator)
        .start()
        .await
        .unwrap();
    let config = ClientConfig::new(format!("http://{}", running.addr()).parse().unwrap());
    let client = EchoServiceClient::new(HttpClient::plaintext(), config);
    let me = client
        .who_am_i(WhoAmIRequest::default())
        .await
        .unwrap()
        .into_owned();
    assert_eq!(me.user_id, "test-user");
    running.shutdown().await.unwrap();
    db.finish().await;
    let _ = Duration::from_secs(0);
}
