//! Real ack loss for an HTTP provider client: a proxy that forwards a
//! request, lets the upstream execute it, and drops the answer.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// The request line a matcher sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHead {
    /// The method, as sent (`POST`).
    pub method: String,
    /// The request target, as sent (`/orders`).
    pub path: String,
}

type Matcher = Arc<dyn Fn(&RequestHead) -> bool + Send + Sync>;

#[derive(Default)]
struct Arm {
    /// The pending drop: its matcher (`None` matches everything) and how
    /// many requests to refuse once it fires.
    armed: Option<(Option<Matcher>, usize)>,
    /// Requests still to refuse after the drop.
    dead: usize,
}

/// An HTTP/1.1 reverse proxy in front of a provider (a simulator on a
/// loopback port) that injects real ack loss: once armed, the next request
/// its matcher accepts is forwarded and executed by the upstream, its
/// response read and discarded, and the client's connection closed without
/// an answer — the request landed, the answer never arrived, with no
/// simulator changes. Point the provider client at [`AckLossProxy::url`]
/// and call [`AckLossProxy::arm`] from the harness's `inject_ambiguity`.
/// Unarmed requests pass through untouched.
///
/// Every forwarded request carries `Connection: close`, so each request
/// rides its own upstream connection and the proxy relays the response by
/// reading to EOF; the client sees the same header and opens a fresh
/// connection next time. The port of the Go `AckLossTransport`, which
/// wrapped the client's round-tripper — reqwest has no such seam, so the
/// injection sits on the wire.
pub struct AckLossProxy {
    url: String,
    arm: Arc<Mutex<Arm>>,
    accept: JoinHandle<()>,
}

impl AckLossProxy {
    /// Starts a proxy on a free loopback port in front of `upstream_url`
    /// (`http://host:port`).
    pub async fn start(upstream_url: &str) -> AckLossProxy {
        let upstream = upstream_url
            .strip_prefix("http://")
            .unwrap_or(upstream_url)
            .trim_end_matches('/')
            .to_owned();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind a loopback port for the ack-loss proxy");
        let addr = listener.local_addr().expect("the listener's address");
        let arm = Arc::new(Mutex::new(Arm::default()));
        let accept = {
            let arm = Arc::clone(&arm);
            tokio::spawn(async move {
                loop {
                    let Ok((client, _)) = listener.accept().await else {
                        return;
                    };
                    tokio::spawn(serve(client, upstream.clone(), Arc::clone(&arm)));
                }
            })
        };
        AckLossProxy {
            url: format!("http://{addr}"),
            arm,
            accept,
        }
    }

    /// The proxy's base URL, for the provider client.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Points the proxy at the next request `matcher` accepts: that request
    /// is delivered and its answer dropped. The `then_refuse` requests after
    /// it are refused WITHOUT being sent — the connection stays dead for
    /// the rest of a send that would otherwise re-read and recover in line
    /// (a create that adopts by name after a failed POST), so the adapter,
    /// not the provider, meets the ambiguity. `then_refuse` is the
    /// provider's re-read count, stated beside each harness that needs one.
    /// One-shot per arm.
    pub fn arm(
        &self,
        matcher: impl Fn(&RequestHead) -> bool + Send + Sync + 'static,
        then_refuse: usize,
    ) {
        let mut arm = self.lock();
        arm.armed = Some((Some(Arc::new(matcher)), then_refuse));
        arm.dead = 0;
    }

    /// Arms the proxy for the very next request, whatever it is, with no
    /// dead requests after it: the one-request form for a provider whose
    /// send is a single request.
    pub fn arm_next(&self) {
        let mut arm = self.lock();
        arm.armed = Some((None, 0));
        arm.dead = 0;
    }

    /// Whether a drop is still pending.
    pub fn is_armed(&self) -> bool {
        self.lock().armed.is_some()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Arm> {
        self.arm.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Drop for AckLossProxy {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

/// One client connection: one request in, one response out (or none).
async fn serve(mut client: TcpStream, upstream: String, arm: Arc<Mutex<Arm>>) {
    let Some((head, request)) = read_request(&mut client).await else {
        return;
    };
    let drop_ack = {
        let mut arm = arm.lock().unwrap_or_else(|p| p.into_inner());
        if arm.dead > 0 {
            arm.dead -= 1;
            // Refused: nothing reaches the upstream; the client sees the
            // connection close before any answer.
            return;
        }
        match &arm.armed {
            Some((matcher, _)) if matcher.as_ref().is_none_or(|m| m(&head)) => {
                let (_, refuse) = arm.armed.take().expect("just matched");
                Some(refuse)
            }
            _ => None,
        }
    };

    let Ok(mut server) = TcpStream::connect(&upstream).await else {
        return;
    };
    if server.write_all(&request).await.is_err() {
        return;
    }
    let mut response = Vec::new();
    let _ = server.read_to_end(&mut response).await;

    match drop_ack {
        Some(refuse) => {
            // The upstream processed the request; its answer stays here, and
            // the connection stays dead for the provider's in-line recovery.
            arm.lock().unwrap_or_else(|p| p.into_inner()).dead = refuse;
        }
        None => {
            let _ = client.write_all(&response).await;
            let _ = client.shutdown().await;
        }
    }
}

/// Reads one HTTP/1.1 request (head and a `Content-Length` body) and
/// returns its request line plus the bytes to forward, with the connection
/// header replaced by `Connection: close`.
async fn read_request(client: &mut TcpStream) -> Option<(RequestHead, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
        let mut chunk = [0u8; 4096];
        match client.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head_text = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head_text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let head = RequestHead {
        method: parts.next()?.to_owned(),
        path: parts.next()?.to_owned(),
    };
    let mut content_length = 0usize;
    let mut forwarded = format!("{request_line}\r\n");
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.eq_ignore_ascii_case("connection") {
            continue;
        }
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().unwrap_or(0);
        }
        forwarded.push_str(line);
        forwarded.push_str("\r\n");
    }
    forwarded.push_str("Connection: close\r\n\r\n");
    let mut body = buf[head_end..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0u8; content_length - body.len()];
        match client.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    let mut request = forwarded.into_bytes();
    request.extend_from_slice(&body);
    Some((head, request))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use basable_externaleffect::is_transport;
    use basable_processingobject_testkit::WidgetSim;

    #[tokio::test]
    async fn an_armed_request_lands_and_its_answer_is_dropped() {
        let sim = WidgetSim::start().await;
        let proxy = AckLossProxy::start(sim.url()).await;
        let client = sim.client_via(proxy.url());

        // Unarmed: pass-through.
        let first = client.place_order("k1", 1).await.unwrap();
        assert_eq!(sim.order_count(), 1);

        // Armed with a matcher that does not match: pass-through, still
        // armed.
        proxy.arm(|head| head.path == "/elsewhere", 0);
        assert_eq!(client.get_order(&first.id).await.unwrap(), first);
        assert!(proxy.is_armed());

        // The matching request lands (a second order exists) but the client
        // sees a transport failure; one more request is refused unsent.
        proxy.arm(|head| head.method == "POST" && head.path == "/orders", 1);
        let err = client.place_order("k2", 2).await.unwrap_err();
        assert!(
            is_transport(&*err),
            "the dropped answer is a transport failure: {err}"
        );
        assert_eq!(sim.order_count(), 2, "the request landed");
        assert!(!proxy.is_armed(), "one-shot");
        let err = client.get_order(&first.id).await.unwrap_err();
        assert!(
            is_transport(&*err),
            "the dead request is refused unsent: {err}"
        );

        // The connection is alive again, and the ack-lost order is there.
        let replay = client.place_order("k2", 2).await.unwrap();
        assert_eq!(replay.widgets, 2);
        assert_eq!(sim.order_count(), 2);
    }
}
