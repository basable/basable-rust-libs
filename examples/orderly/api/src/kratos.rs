//! What this app's Kratos calls on the app. One route: the pre-persist hook on
//! a registration by passkey (`selfservice.flows.registration.after.passkey`
//! in `k8s/kratos/configmap.yaml`), which refuses every one. A passkey proves
//! no email, so an account made by one could hold someone else's address, and
//! that person's code sign-in would land in it with the stranger's passkey
//! still on it. Kratos has no switch for "sign-in and settings only".
//!
//! Kratos calls with no session, so the auth layer lets this prefix through
//! (`app/src/main.rs`). The answer is a constant: a caller that reaches it
//! from outside learns nothing and changes nothing.

use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;

/// The paths Kratos calls, public for the auth layer.
pub const PUBLIC_PREFIX: &str = "/internal/kratos/";

/// Kratos's web hook error format: a validation message on the email field,
/// which the registration page shows like any other.
const PASSKEY_REGISTRATION_REFUSAL: &str = r##"{"messages":[{"instance_ptr":"#/traits/email","messages":[{"id":4000001,"text":"Sign up with an email code first.","type":"error","context":{}}]}]}"##;

/// The routes Kratos calls, mounted beside the webhooks.
pub fn routes() -> Router {
    Router::new().route(
        "/internal/kratos/refuse-passkey-registration",
        post(refuse_passkey_registration),
    )
}

async fn refuse_passkey_registration() -> impl IntoResponse {
    (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE, "application/json")],
        PASSKEY_REGISTRATION_REFUSAL,
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;

    use basable_auth::Validator;

    use super::*;

    /// Through the auth layer, as `app/src/main.rs` mounts it: Kratos has no
    /// session, and a 401 here would lose the message the person should see.
    #[tokio::test]
    async fn refuses_a_caller_with_no_session() {
        let validator =
            Arc::new(Validator::kratos("http://127.0.0.1:9").public_prefixes([PUBLIC_PREFIX]));
        let routes = validator.apply(routes());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, routes).await });

        let answer = tokio::task::spawn_blocking(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .write_all(
                    b"POST /internal/kratos/refuse-passkey-registration HTTP/1.1\r\n\
                      Host: app\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            answer
        })
        .await
        .unwrap();

        assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
        let (_, body) = answer.split_once("\r\n\r\n").unwrap();
        let refusal: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(refusal["messages"][0]["instance_ptr"], "#/traits/email");
        assert_eq!(refusal["messages"][0]["messages"][0]["type"], "error");
    }
}
