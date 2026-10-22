//! Ory Kratos session validation, the port of the monorepo's
//! `golang/lib/auth`. A [`Validator`] is applied to the app's axum router
//! ([`Validator::apply`]); every request that is not on a public path must
//! carry a session Kratos accepts, or it is answered with a Connect-shaped
//! `401 unauthenticated`. The session is the cookie a browser holds or the
//! session token a native app holds after an API login flow, sent in
//! [`SESSION_TOKEN_HEADER`] ([`Credential`]). Kratos being unreachable is a
//! `503 unavailable`, never a silent allow. A validated request carries
//! its [`Identity`] in the request extensions, from where
//! `basable-connect` puts it on the handler's `Ctx` ([`AuthCtx`]).
//!
//! The [`SessionHook`] fires after every successful validation, on the
//! request path: the seam for first-touch user provisioning. It must not
//! fail the request and must be quick on the common path.
//!
//! Sessions can slide ([`Validator::extend_sessions`]): a validated session
//! with less than a window left is extended through Kratos's admin API, so
//! it ends after a whole `session.lifespan` without use instead of a
//! lifespan after its sign-in.
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

/// How long a rejected credential stays rejected without asking Kratos
/// again.
/// Small: a session that just became valid waits at most this long.
pub const NEGATIVE_WINDOW: Duration = Duration::from_secs(5);

/// The negative cache's size bound; it is cleared when full.
const NEGATIVE_CAPACITY: usize = 4096;

/// Where and when sessions are extended ([`Validator::extend_sessions`]).
struct Extension {
    admin: String,
    within: Duration,
}

/// What a validated session's extension needs: its id and expiry.
struct SessionTiming {
    id: String,
    expires_at: Option<chrono::DateTime<chrono::FixedOffset>>,
}

/// The header a native app sends its Kratos session token in
/// (`X-Session-Token`): what an API (native) login flow hands out where a
/// browser flow sets the cookie. Kratos's `/sessions/whoami` takes either.
pub const SESSION_TOKEN_HEADER: &str = "x-session-token";

/// What a request presents for its session, passed to Kratos in the header
/// it came in. Its `Debug` never prints the secret.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Credential {
    /// A Kratos session token ([`SESSION_TOKEN_HEADER`]), what a native app
    /// holds.
    Token(String),
    /// The `Cookie` header, what a browser holds.
    Cookie(String),
}

impl Credential {
    /// The credential `headers` carry: the session token when there is one,
    /// the cookie otherwise. The token decides when both are sent, as it
    /// does in Kratos itself, so a refused token is never rescued by a
    /// cookie beside it; a token that is not even text (bytes above ASCII,
    /// which a header may carry) is still the token presented, and Kratos
    /// refuses it, as Go's `Header.Get` handed it on. `None` when there is
    /// neither (a blank token counts as none).
    pub fn from_headers(headers: &http::HeaderMap) -> Option<Credential> {
        let token = headers
            .get(SESSION_TOKEN_HEADER)
            .map(|v| String::from_utf8_lossy(v.as_bytes()))
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        if let Some(token) = token {
            return Some(Credential::Token(token));
        }
        let cookie = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>()
            .join("; ");
        (!cookie.is_empty()).then_some(Credential::Cookie(cookie))
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Credential::Token(_) => f.write_str("Token(..)"),
            Credential::Cookie(_) => f.write_str("Cookie(..)"),
        }
    }
}

/// The authenticated caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The Kratos identity id.
    pub id: String,
    /// The `traits.email` of the identity schema, empty when absent.
    pub email: String,
    /// Whether Kratos marked that address verified (an email code typed
    /// back proves it; a passkey registration or a settings change does
    /// not). See [`KratosIdentity::email_verified`].
    pub email_verified: bool,
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
    /// When the session ends unless it is extended (RFC 3339).
    #[serde(default)]
    pub expires_at: String,
}

/// The identity in a Kratos session.
#[derive(Debug, Deserialize)]
pub struct KratosIdentity {
    /// The identity id.
    pub id: String,
    /// The schema's traits.
    #[serde(default)]
    pub traits: KratosTraits,
    /// The identity's addresses and whether Kratos has seen each proven.
    #[serde(default)]
    pub verifiable_addresses: Vec<KratosVerifiableAddress>,
}

/// One of an identity's verifiable addresses.
#[derive(Debug, Default, Deserialize)]
pub struct KratosVerifiableAddress {
    /// The address.
    #[serde(default)]
    pub value: String,
    /// How it is verified: `email` for an email address.
    #[serde(default)]
    pub via: String,
    /// Whether Kratos saw it proven.
    #[serde(default)]
    pub verified: bool,
}

impl KratosIdentity {
    /// Whether the email trait is an address Kratos marked verified,
    /// compared without case. An address set without proof is not.
    pub fn email_verified(&self) -> bool {
        let email = &self.traits.email;
        !email.is_empty()
            && self
                .verifiable_addresses
                .iter()
                .any(|a| a.via == "email" && a.verified && a.value.eq_ignore_ascii_case(email))
    }
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
    /// Neither a session token nor a cookie.
    NoSession,
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
            AuthError::NoSession | AuthError::Rejected(_) => StatusCode::UNAUTHORIZED,
            AuthError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// The Connect error code name.
    pub fn code(&self) -> &'static str {
        match self {
            AuthError::NoSession | AuthError::Rejected(_) => "unauthenticated",
            AuthError::Unavailable(_) => "unavailable",
        }
    }

    /// The message the client sees. It never repeats what Kratos said
    /// about the session.
    pub fn message(&self) -> &'static str {
        match self {
            AuthError::NoSession => "authentication required",
            AuthError::Rejected(_) => "invalid session",
            AuthError::Unavailable(_) => "authentication unavailable",
        }
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::NoSession => f.write_str("no session token or cookie"),
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
    extension: Option<Extension>,
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
            extension: None,
        }
    }

    /// Makes sessions slide, as Go's `WithSessionExtension`: a validated
    /// session with less than `within` left is extended through Kratos's
    /// admin API at `admin_url` (`PATCH /admin/sessions/{id}/extend`, the
    /// in-cluster admin port, never public), so it ends after a whole
    /// `session.lifespan` without use. `within` must equal Kratos's
    /// `session.earliest_possible_extend`, before which Kratos ignores an
    /// extension. Best effort: a failed extension is logged and the request
    /// goes on. The browser's cookie follows at its next whoami, which
    /// re-issues it for the renewed session.
    pub fn extend_sessions(mut self, admin_url: &str, within: Duration) -> Validator {
        assert!(
            !admin_url.is_empty() && !within.is_zero(),
            "extend_sessions needs Kratos's admin URL and a positive window"
        );
        self.extension = Some(Extension {
            admin: admin_url.trim_end_matches('/').to_string(),
            within,
        });
        self
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

    /// Validates a request's credential ([`Credential::from_headers`]). On
    /// success the hook has fired, and the session was extended if it was
    /// due ([`Validator::extend_sessions`]).
    pub async fn authenticate(
        &self,
        credential: Option<&Credential>,
    ) -> Result<Identity, AuthError> {
        let (identity, due) = match &self.mode {
            #[cfg(feature = "test-bypass")]
            Mode::Bypass(identity) => (identity.clone(), None),
            Mode::Kratos { whoami, client } => {
                let credential = credential.ok_or(AuthError::NoSession)?;
                if self.recently_rejected(credential) {
                    return Err(AuthError::Rejected("recently rejected".to_string()));
                }
                match whoami_call(client, whoami, credential).await {
                    Ok((identity, timing)) => (identity, Some((client, timing))),
                    Err(e @ AuthError::Rejected(_)) => {
                        self.remember_rejected(credential);
                        return Err(e);
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        if let Some(hook) = &self.hook {
            hook(&identity);
        }
        if let Some((client, timing)) = due {
            self.extend_if_due(client, &timing).await;
        }
        Ok(identity)
    }

    /// Extends a session with less than the window left. A failure is
    /// logged: the session is valid either way, and its next request tries
    /// again.
    async fn extend_if_due(&self, client: &reqwest::Client, session: &SessionTiming) {
        let (Some(extension), Some(expires_at)) = (&self.extension, session.expires_at) else {
            return;
        };
        // Kratos's session ids are UUIDs: anything else is not put in a path.
        if session.id.is_empty()
            || !session
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return;
        }
        let left = expires_at.signed_duration_since(chrono::Utc::now());
        match chrono::TimeDelta::from_std(extension.within) {
            Ok(within) if left < within => {}
            _ => return,
        }
        let url = format!("{}/admin/sessions/{}/extend", extension.admin, session.id);
        match client
            .patch(&url)
            .header(header::ACCEPT, "application/json")
            .send()
            .await
        {
            Ok(resp)
                if resp.status() == StatusCode::NO_CONTENT || resp.status() == StatusCode::OK =>
            {
                tracing::debug!(session = %session.id, "session extended");
            }
            Ok(resp) => tracing::warn!(
                session = %session.id,
                status = %resp.status(),
                "extending the session failed; the next request tries again"
            ),
            Err(e) => tracing::warn!(
                session = %session.id,
                error = %e,
                "extending the session failed; the next request tries again"
            ),
        }
    }

    fn recently_rejected(&self, credential: &Credential) -> bool {
        let key = credential_key(credential);
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

    fn remember_rejected(&self, credential: &Credential) {
        let mut negative = self.negative.lock().expect("negative cache lock");
        if negative.len() >= NEGATIVE_CAPACITY {
            negative.clear();
        }
        negative.insert(credential_key(credential), Instant::now());
    }

    /// Wraps every route of `router` with this validator: public paths
    /// pass, the rest need a session, and a validated request carries its
    /// [`Identity`] in the request extensions.
    pub fn apply(self: Arc<Validator>, router: axum::Router) -> axum::Router {
        router.layer(axum::middleware::from_fn_with_state(self, authenticate))
    }
}

/// The negative cache's key: the credential's kind and value, so a token
/// and a cookie of the same text are different entries.
fn credential_key(credential: &Credential) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    credential.hash(&mut h);
    h.finish()
}

async fn whoami_call(
    client: &reqwest::Client,
    whoami: &str,
    credential: &Credential,
) -> Result<(Identity, SessionTiming), AuthError> {
    let (name, value) = match credential {
        Credential::Token(token) => (SESSION_TOKEN_HEADER, token),
        Credential::Cookie(cookie) => (header::COOKIE.as_str(), cookie),
    };
    let resp = client
        .get(whoami)
        .header(name, value)
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
    let email_verified = session.identity.email_verified();
    let timing = SessionTiming {
        expires_at: chrono::DateTime::parse_from_rfc3339(&session.expires_at).ok(),
        id: session.id,
    };
    Ok((
        Identity {
            id: session.identity.id,
            email: session.identity.traits.email,
            email_verified,
        },
        timing,
    ))
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
    let credential = Credential::from_headers(req.headers());
    match validator.authenticate(credential.as_ref()).await {
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

    // A session's email counts as verified only when Kratos marked that
    // very address verified, as Go's KratosIdentity.EmailVerified does.
    #[test]
    fn the_email_is_verified_only_when_kratos_says_so() {
        let identity = |email: &str, addresses: &[(&str, bool)]| -> KratosIdentity {
            serde_json::from_value(serde_json::json!({
                "id": "k",
                "traits": { "email": email },
                "verifiable_addresses": addresses
                    .iter()
                    .map(|(v, ok)| serde_json::json!({ "value": v, "via": "email", "verified": ok }))
                    .collect::<Vec<_>>(),
            }))
            .unwrap()
        };
        assert!(identity("a@x.test", &[("a@x.test", true)]).email_verified());
        assert!(identity("A@x.test", &[("a@x.test", true)]).email_verified());
        assert!(!identity("a@x.test", &[("a@x.test", false)]).email_verified());
        assert!(!identity("b@x.test", &[("a@x.test", true)]).email_verified());
        assert!(!identity("a@x.test", &[]).email_verified());
        assert!(!identity("", &[("", true)]).email_verified());
    }

    fn headers(pairs: &[(&str, &str)]) -> http::HeaderMap {
        let mut h = http::HeaderMap::new();
        for (k, v) in pairs {
            let name = http::HeaderName::from_bytes(k.as_bytes()).unwrap();
            h.append(name, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn the_token_decides_over_the_cookie() {
        let token = || Credential::Token("t-1".into());
        assert_eq!(
            Credential::from_headers(&headers(&[("x-session-token", "t-1")])),
            Some(token())
        );
        assert_eq!(
            Credential::from_headers(&headers(&[
                ("cookie", "ory_kratos_session=c"),
                ("X-Session-Token", " t-1 "),
            ])),
            Some(token()),
            "the token wins, trimmed, whatever the header's case"
        );
        assert_eq!(
            Credential::from_headers(&headers(&[("cookie", "a=1"), ("cookie", "b=2")])),
            Some(Credential::Cookie("a=1; b=2".into())),
            "split cookie headers are joined"
        );
        assert_eq!(
            Credential::from_headers(&headers(&[("x-session-token", "  "), ("cookie", "a=1")])),
            Some(Credential::Cookie("a=1".into())),
            "a blank token is no token"
        );
        assert_eq!(
            Credential::from_headers(&headers(&[("x-session-token", "")])),
            None
        );
        assert_eq!(Credential::from_headers(&http::HeaderMap::new()), None);

        // A token that is not text is still the token presented: it goes to
        // Kratos (which refuses it) rather than letting the cookie decide.
        let mut h = headers(&[("cookie", "ory_kratos_session=c")]);
        h.insert(
            SESSION_TOKEN_HEADER,
            http::HeaderValue::from_bytes(b"t\xff1").unwrap(),
        );
        assert!(
            matches!(Credential::from_headers(&h), Some(Credential::Token(_))),
            "a non-text token is not rescued by the cookie"
        );
    }

    #[test]
    fn a_credential_never_prints_its_secret() {
        assert_eq!(
            format!("{:?}", Credential::Token("secret".into())),
            "Token(..)"
        );
        assert_eq!(
            format!("{:?}", Credential::Cookie("secret".into())),
            "Cookie(..)"
        );
    }

    /// A fake Kratos on loopback: whoami answers a session expiring in
    /// `expires_in`, the admin API counts its extensions.
    async fn fake_kratos(expires_in: chrono::TimeDelta) -> (String, Arc<Mutex<Vec<String>>>) {
        let extended = Arc::new(Mutex::new(Vec::new()));
        let seen = extended.clone();
        let expires_at = (chrono::Utc::now() + expires_in).to_rfc3339();
        let app = axum::Router::new()
            .route(
                "/sessions/whoami",
                axum::routing::get(move || {
                    let expires_at = expires_at.clone();
                    async move {
                        axum::Json(serde_json::json!({
                            "id": "8d2c1c3e-1111-4222-8333-444455556666",
                            "active": true,
                            "expires_at": expires_at,
                            "identity": { "id": "kratos-1", "traits": { "email": "u@example.com" } },
                        }))
                    }
                }),
            )
            .route(
                "/admin/sessions/{id}/extend",
                axum::routing::patch(move |axum::extract::Path(id): axum::extract::Path<String>| {
                    let seen = seen.clone();
                    async move {
                        seen.lock().unwrap().push(id);
                        StatusCode::NO_CONTENT
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, extended)
    }

    // A session with less than the window left is extended through the
    // admin API, one with more is left alone, and nothing is extended
    // without the option: Go's TestSessionExtension.
    #[tokio::test]
    async fn a_due_session_is_extended_through_the_admin_api() {
        let window = Duration::from_secs(29 * 24 * 3600);
        let cookie = Credential::Cookie("ory_kratos_session=s".into());

        let (url, extended) =
            fake_kratos(chrono::TimeDelta::days(29) - chrono::TimeDelta::minutes(1)).await;
        let v = Validator::kratos(&url).extend_sessions(&format!("{url}/"), window);
        v.authenticate(Some(&cookie)).await.unwrap();
        assert_eq!(
            *extended.lock().unwrap(),
            ["8d2c1c3e-1111-4222-8333-444455556666"]
        );

        let (url, extended) = fake_kratos(chrono::TimeDelta::days(30)).await;
        let v = Validator::kratos(&url).extend_sessions(&url, window);
        v.authenticate(Some(&cookie)).await.unwrap();
        assert!(extended.lock().unwrap().is_empty());

        let (url, extended) = fake_kratos(chrono::TimeDelta::minutes(1)).await;
        Validator::kratos(&url)
            .authenticate(Some(&cookie))
            .await
            .unwrap();
        assert!(extended.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_missing_session_is_unauthenticated_without_asking_kratos() {
        let v = Validator::kratos("http://127.0.0.1:1");
        assert_eq!(v.authenticate(None).await, Err(AuthError::NoSession));
        assert_eq!(AuthError::NoSession.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(AuthError::NoSession.code(), "unauthenticated");
    }

    #[tokio::test]
    async fn an_unreachable_kratos_is_unavailable_never_allowed() {
        let v = Validator::kratos("http://127.0.0.1:1");
        for credential in [
            Credential::Cookie("ory_kratos_session=x".into()),
            Credential::Token("x".into()),
        ] {
            let err = v.authenticate(Some(&credential)).await.unwrap_err();
            assert!(matches!(err, AuthError::Unavailable(_)), "{err}");
            assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    #[tokio::test]
    async fn the_bypass_authenticates_everything_and_fires_the_hook() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let v = Validator::bypassed_for_tests(Identity {
            id: "user-1".into(),
            email: "u@example.com".into(),
            email_verified: true,
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
        let cookie = |s: &str| Credential::Cookie(s.into());
        assert!(!v.recently_rejected(&cookie("a")));
        v.remember_rejected(&cookie("a"));
        assert!(v.recently_rejected(&cookie("a")));
        assert!(!v.recently_rejected(&cookie("b")));
        assert!(
            !v.recently_rejected(&Credential::Token("a".into())),
            "a token is not the cookie of the same text"
        );
        v.negative.lock().unwrap().insert(
            credential_key(&cookie("a")),
            Instant::now() - NEGATIVE_WINDOW - Duration::from_secs(1),
        );
        assert!(!v.recently_rejected(&cookie("a")));
        for i in 0..NEGATIVE_CAPACITY {
            v.remember_rejected(&cookie(&i.to_string()));
        }
        assert!(v.negative.lock().unwrap().len() <= NEGATIVE_CAPACITY);
        v.remember_rejected(&cookie("overflow"));
        assert_eq!(v.negative.lock().unwrap().len(), 1);
    }
}
