//! Ory Kratos session validation, the port of the monorepo's
//! `golang/lib/auth`. A [`Validator`] is applied to the app's axum router
//! ([`Validator::apply`]); every request that is not on a public path must
//! carry a session cookie Kratos accepts, or it is answered with a
//! Connect-shaped `401 unauthenticated`. Kratos being unreachable is a
//! `503 unavailable`, never a silent allow. A validated request carries
//! its [`Identity`] in the request extensions, from where
//! `basable-connect` puts it on the handler's `Ctx` ([`AuthCtx`]).
//!
//! The [`SessionHook`] fires after every successful validation, on the
//! request path: the seam for first-touch user provisioning. It must not
//! fail the request and must be quick on the common path.
//!
//! The test bypass ([`Validator::bypassed_for_tests`]) exists only behind
//! the `test-bypass` feature, which `basable-testkit` enables.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use basable_core::Ctx;
use serde::Deserialize;

/// Bounds one call to Kratos.
pub const KRATOS_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a rejected cookie stays rejected without asking Kratos again.
/// Small: a session that just became valid waits at most this long.
pub const NEGATIVE_WINDOW: Duration = Duration::from_secs(5);

/// The negative cache's size bound; it is cleared when full.
const NEGATIVE_CAPACITY: usize = 4096;

/// The authenticated caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The Kratos identity id.
    pub id: String,
    /// The `traits.email` of the identity schema, empty when absent.
    pub email: String,
}

/// The response of `GET /sessions/whoami`.
#[derive(Debug, Deserialize)]
pub struct KratosSession {
    /// The session id.
    #[serde(default)]
    pub id: String,
    /// Whether the session is active.
    #[serde(default)]
    pub active: bool,
    /// The identity.
    pub identity: KratosIdentity,
}

/// The identity in a Kratos session.
#[derive(Debug, Deserialize)]
pub struct KratosIdentity {
    /// The identity id.
    pub id: String,
    /// The schema's traits.
    #[serde(default)]
    pub traits: KratosTraits,
}

/// The traits the platform's identity schema defines.
#[derive(Debug, Default, Deserialize)]
pub struct KratosTraits {
    /// The login identifier.
    #[serde(default)]
    pub email: String,
}

/// Runs after every successful validation with the identity.
pub type SessionHook = Arc<dyn Fn(&Identity) + Send + Sync>;

/// Why a request was not authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// No `Cookie` header.
    NoCookie,
    /// Kratos rejected the session (401), or it is inactive or empty.
    Rejected(String),
    /// Kratos could not be asked: transport failure, timeout, or an
    /// unexpected status.
    Unavailable(String),
}

impl AuthError {
    /// The HTTP status: 401 for a rejected or missing session, 503 when
    /// Kratos could not be asked.
    pub fn status(&self) -> StatusCode {
        match self {
            AuthError::NoCookie | AuthError::Rejected(_) => StatusCode::UNAUTHORIZED,
            AuthError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// The Connect error code name.
    pub fn code(&self) -> &'static str {
        match self {
            AuthError::NoCookie | AuthError::Rejected(_) => "unauthenticated",
            AuthError::Unavailable(_) => "unavailable",
        }
    }

    /// The message the client sees. It never repeats what Kratos said
    /// about the session.
    pub fn message(&self) -> &'static str {
        match self {
            AuthError::NoCookie => "authentication required",
            AuthError::Rejected(_) => "invalid session",
            AuthError::Unavailable(_) => "authentication unavailable",
        }
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::NoCookie => f.write_str("no session cookie"),
            AuthError::Rejected(why) => write!(f, "session rejected: {why}"),
            AuthError::Unavailable(why) => write!(f, "kratos unavailable: {why}"),
        }
    }
}

impl std::error::Error for AuthError {}

impl IntoResponse for AuthError {
    /// The Connect protocol's JSON error shape, so a Connect client and a
    /// browser both read it.
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "code": self.code(), "message": self.message() });
        (
            self.status(),
            [(header::CONTENT_TYPE, "application/json")],
            body.to_string(),
        )
            .into_response()
    }
}

enum Mode {
    Kratos {
        whoami: String,
        client: reqwest::Client,
    },
    #[cfg(feature = "test-bypass")]
    Bypass(Identity),
}

/// The session validator.
pub struct Validator {
    mode: Mode,
    public_paths: HashSet<String>,
    public_prefixes: Vec<String>,
    hook: Option<SessionHook>,
    negative: Mutex<HashMap<u64, Instant>>,
}

impl fmt::Debug for Validator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mode = match &self.mode {
            Mode::Kratos { whoami, .. } => whoami.as_str(),
            #[cfg(feature = "test-bypass")]
            Mode::Bypass(_) => "bypass",
        };
        f.debug_struct("Validator")
            .field("mode", &mode)
            .field("public_paths", &self.public_paths)
            .field("public_prefixes", &self.public_prefixes)
            .finish_non_exhaustive()
    }
}

impl Validator {
    /// A validator asking Kratos's public API at `url` (`http://kratos-public:4433`).
    /// `/healthz` and `/readyz` are public from the start.
    pub fn kratos(url: &str) -> Validator {
        let client = reqwest::Client::builder()
            .timeout(KRATOS_TIMEOUT)
            .build()
            .expect("a reqwest client with a timeout builds");
        Validator::with_mode(Mode::Kratos {
            whoami: format!("{}/sessions/whoami", url.trim_end_matches('/')),
            client,
        })
    }

    /// A validator that authenticates every non-public request as
    /// `identity` without asking anyone. Tests only: `basable-testkit`
    /// enables the feature; production wiring cannot name this.
    #[cfg(feature = "test-bypass")]
    pub fn bypassed_for_tests(identity: Identity) -> Validator {
        Validator::with_mode(Mode::Bypass(identity))
    }

    fn with_mode(mode: Mode) -> Validator {
        Validator {
            mode,
            public_paths: ["/healthz", "/readyz"]
                .into_iter()
                .map(String::from)
                .collect(),
            public_prefixes: Vec::new(),
            hook: None,
            negative: Mutex::new(HashMap::new()),
        }
    }

    /// Adds exact paths that bypass authentication.
    pub fn public_paths<I, S>(mut self, paths: I) -> Validator
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.public_paths.extend(paths.into_iter().map(Into::into));
        self
    }

    /// Adds path prefixes that bypass authentication (`/api/webhooks/`).
    pub fn public_prefixes<I, S>(mut self, prefixes: I) -> Validator
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.public_prefixes
            .extend(prefixes.into_iter().map(Into::into));
        self
    }

    /// Registers the session hook.
    pub fn session_hook(mut self, hook: impl Fn(&Identity) + Send + Sync + 'static) -> Validator {
        self.hook = Some(Arc::new(hook));
        self
    }

    /// Whether `path` bypasses authentication.
    pub fn is_public(&self, path: &str) -> bool {
        self.public_paths.contains(path) || self.public_prefixes.iter().any(|p| path.starts_with(p))
    }

    /// Validates a request's `Cookie` header value. On success the hook
    /// has fired.
    pub async fn authenticate(&self, cookie: Option<&str>) -> Result<Identity, AuthError> {
        let identity = match &self.mode {
            #[cfg(feature = "test-bypass")]
            Mode::Bypass(identity) => identity.clone(),
            Mode::Kratos { whoami, client } => {
                let cookie = cookie.ok_or(AuthError::NoCookie)?;
                if self.recently_rejected(cookie) {
                    return Err(AuthError::Rejected("recently rejected".to_string()));
                }
                match whoami_call(client, whoami, cookie).await {
                    Ok(identity) => identity,
                    Err(e @ AuthError::Rejected(_)) => {
                        self.remember_rejected(cookie);
                        return Err(e);
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        if let Some(hook) = &self.hook {
            hook(&identity);
        }
        Ok(identity)
    }

    fn recently_rejected(&self, cookie: &str) -> bool {
        let key = cookie_key(cookie);
        let mut negative = self.negative.lock().expect("negative cache lock");
        match negative.get(&key) {
            Some(at) if at.elapsed() < NEGATIVE_WINDOW => true,
            Some(_) => {
                negative.remove(&key);
                false
            }
            None => false,
        }
    }

    fn remember_rejected(&self, cookie: &str) {
        let mut negative = self.negative.lock().expect("negative cache lock");
        if negative.len() >= NEGATIVE_CAPACITY {
            negative.clear();
        }
        negative.insert(cookie_key(cookie), Instant::now());
    }

    /// Wraps every route of `router` with this validator: public paths
    /// pass, the rest need a session, and a validated request carries its
    /// [`Identity`] in the request extensions.
    pub fn apply(self: Arc<Validator>, router: axum::Router) -> axum::Router {
        router.layer(axum::middleware::from_fn_with_state(self, authenticate))
    }
}

fn cookie_key(cookie: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cookie.hash(&mut h);
    h.finish()
}

async fn whoami_call(
    client: &reqwest::Client,
    whoami: &str,
    cookie: &str,
) -> Result<Identity, AuthError> {
    let resp = client
        .get(whoami)
        .header(header::COOKIE, cookie)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| AuthError::Unavailable(e.to_string()))?;
    match resp.status() {
        StatusCode::OK => {}
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err(AuthError::Rejected(format!(
                "kratos answered {}",
                resp.status()
            )));
        }
        other => {
            return Err(AuthError::Unavailable(format!("kratos answered {other}")));
        }
    }
    let session: KratosSession = resp
        .json()
        .await
        .map_err(|e| AuthError::Unavailable(format!("kratos answer does not parse: {e}")))?;
    if !session.active {
        return Err(AuthError::Rejected("session is not active".to_string()));
    }
    if session.identity.id.is_empty() {
        return Err(AuthError::Rejected(
            "session has no identity id".to_string(),
        ));
    }
    Ok(Identity {
        id: session.identity.id,
        email: session.identity.traits.email,
    })
}

async fn authenticate(
    State(validator): State<Arc<Validator>>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    if validator.is_public(&path) {
        return next.run(req).await;
    }
    let cookie = req
        .headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join("; ");
    let cookie = if cookie.is_empty() {
        None
    } else {
        Some(cookie.as_str())
    };
    match validator.authenticate(cookie).await {
        Ok(identity) => {
            req.extensions_mut().insert(identity);
            next.run(req).await
        }
        Err(e) => {
            tracing::warn!(path, error = %e, "request not authenticated");
            e.into_response()
        }
    }
}

/// The identity a validated request carries.
pub fn identity_of(extensions: &http::Extensions) -> Option<&Identity> {
    extensions.get::<Identity>()
}

/// The identity on a handler's context.
pub trait AuthCtx {
    /// The authenticated caller, if the request was validated.
    fn identity(&self) -> Option<&Identity>;
    /// The caller's Kratos id.
    fn user_id(&self) -> Option<&str> {
        self.identity().map(|i| i.id.as_str())
    }
}

impl AuthCtx for Ctx {
    fn identity(&self) -> Option<&Identity> {
        self.value::<Identity>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_paths_and_prefixes_bypass() {
        let v = Validator::kratos("http://kratos:4433/")
            .public_paths(["/login"])
            .public_prefixes(["/api/webhooks/"]);
        assert!(v.is_public("/healthz"));
        assert!(v.is_public("/readyz"));
        assert!(v.is_public("/login"));
        assert!(v.is_public("/api/webhooks/github"));
        assert!(!v.is_public("/api/webhooks"));
        assert!(!v.is_public("/x.v1.Service/Method"));
        if let Mode::Kratos { whoami, .. } = &v.mode {
            assert_eq!(whoami, "http://kratos:4433/sessions/whoami");
        }
    }

    #[tokio::test]
    async fn a_missing_cookie_is_unauthenticated_without_asking_kratos() {
        let v = Validator::kratos("http://127.0.0.1:1");
        assert_eq!(v.authenticate(None).await, Err(AuthError::NoCookie));
        assert_eq!(AuthError::NoCookie.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(AuthError::NoCookie.code(), "unauthenticated");
    }

    #[tokio::test]
    async fn an_unreachable_kratos_is_unavailable_never_allowed() {
        let v = Validator::kratos("http://127.0.0.1:1");
        let err = v
            .authenticate(Some("ory_kratos_session=x"))
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::Unavailable(_)), "{err}");
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn the_bypass_authenticates_everything_and_fires_the_hook() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let v = Validator::bypassed_for_tests(Identity {
            id: "user-1".into(),
            email: "u@example.com".into(),
        })
        .session_hook(move |i| s.lock().unwrap().push(i.id.clone()));
        let id = v.authenticate(None).await.unwrap();
        assert_eq!(id.id, "user-1");
        assert_eq!(seen.lock().unwrap().as_slice(), ["user-1"]);
        let ctx = Ctx::background().with_value(id);
        assert_eq!(ctx.user_id(), Some("user-1"));
    }

    #[test]
    fn the_negative_cache_expires_and_bounds_itself() {
        let v = Validator::kratos("http://127.0.0.1:1");
        assert!(!v.recently_rejected("a"));
        v.remember_rejected("a");
        assert!(v.recently_rejected("a"));
        assert!(!v.recently_rejected("b"));
        v.negative.lock().unwrap().insert(
            cookie_key("a"),
            Instant::now() - NEGATIVE_WINDOW - Duration::from_secs(1),
        );
        assert!(!v.recently_rejected("a"));
        for i in 0..NEGATIVE_CAPACITY {
            v.remember_rejected(&i.to_string());
        }
        assert!(v.negative.lock().unwrap().len() <= NEGATIVE_CAPACITY);
        v.remember_rejected("overflow");
        assert_eq!(v.negative.lock().unwrap().len(), 1);
    }
}
