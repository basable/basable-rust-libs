//! A TCP proxy that drops one COMMIT acknowledgement.
//!
//! The framework-neutral port of the processing-object testkit's
//! `commitfault.go`: conformance suites prove both ambiguous-commit cases
//! without production-only hooks. Go wrapped the driver's dial function;
//! sqlx has no such seam, so this is a socket in front of Postgres that a
//! pool is pointed at.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// Which ambiguity the next COMMIT suffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitFault {
    /// The COMMIT reaches Postgres and the transaction durably lands, but
    /// the acknowledgement is dropped and the session closed: the applied
    /// ambiguity.
    Applied,
    /// The COMMIT is rewritten to a `SELECT` of the same length and the
    /// session closed with the transaction open, so Postgres rolls it back:
    /// the rolled-back ambiguity, indistinguishable on the wire.
    RolledBack,
}

const NONE: u8 = 0;
const APPLIED: u8 = 1;
const ROLLED_BACK: u8 = 2;

/// The proxy. Every connection through it relays bytes unchanged until the
/// proxy is armed; the first COMMIT after that, on whichever connection
/// carries it, faults once, and the proxy disarms itself.
pub struct CommitFaultProxy {
    addr: SocketAddr,
    armed: Arc<AtomicU8>,
    accept: JoinHandle<()>,
}

impl CommitFaultProxy {
    /// Listens on a free loopback port and forwards to `upstream`.
    pub async fn start(upstream: SocketAddr) -> io::Result<CommitFaultProxy> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let armed = Arc::new(AtomicU8::new(NONE));
        let accept = tokio::spawn({
            let armed = Arc::clone(&armed);
            async move {
                loop {
                    let Ok((client, _)) = listener.accept().await else {
                        break;
                    };
                    let armed = Arc::clone(&armed);
                    tokio::spawn(async move {
                        if let Ok(server) = TcpStream::connect(upstream).await {
                            relay(client, server, armed).await;
                        }
                    });
                }
            }
        });
        Ok(CommitFaultProxy {
            addr,
            armed,
            accept,
        })
    }

    /// The proxy from the host and port of `options`, and those options
    /// re-pointed at the proxy with TLS off (a proxy cannot relay a TLS
    /// handshake it does not terminate).
    pub async fn for_options(
        options: PgConnectOptions,
    ) -> io::Result<(CommitFaultProxy, PgConnectOptions)> {
        let host = options.get_host().to_owned();
        let port = options.get_port();
        let upstream = tokio::net::lookup_host((host.as_str(), port))
            .await?
            .next()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{host}:{port} did not resolve"),
                )
            })?;
        let proxy = CommitFaultProxy::start(upstream).await?;
        let options = options
            .host(&proxy.addr.ip().to_string())
            .port(proxy.addr.port())
            .ssl_mode(PgSslMode::Disable);
        Ok((proxy, options))
    }

    /// The address a pool connects to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Arms the next COMMIT to fault in `mode`.
    pub fn arm(&self, mode: CommitFault) {
        self.armed.store(
            match mode {
                CommitFault::Applied => APPLIED,
                CommitFault::RolledBack => ROLLED_BACK,
            },
            Ordering::SeqCst,
        );
    }

    /// Whether a fault is still pending (no COMMIT has passed since `arm`).
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst) != NONE
    }
}

impl Drop for CommitFaultProxy {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

/// Relays one connection. Client bytes are inspected for the COMMIT command
/// while armed; once a fault fires, server bytes are swallowed up to the
/// ReadyForQuery that ends the command and both sockets are closed.
async fn relay(client: TcpStream, server: TcpStream, armed: Arc<AtomicU8>) {
    let (mut client_rd, mut client_wr) = client.into_split();
    let (mut server_rd, mut server_wr) = server.into_split();
    let dropping = Arc::new(AtomicU8::new(0));

    let upstream = {
        let dropping = Arc::clone(&dropping);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let n = match client_rd.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let chunk = &mut buf[..n];
                if let Some(at) = find_commit(chunk) {
                    match armed.swap(NONE, Ordering::SeqCst) {
                        NONE => {}
                        mode => {
                            if mode == ROLLED_BACK {
                                // Same length as COMMIT: the message frame stays valid.
                                chunk[at..at + 6].copy_from_slice(b"select");
                            }
                            dropping.store(1, Ordering::SeqCst);
                        }
                    }
                }
                if server_wr.write_all(chunk).await.is_err() {
                    break;
                }
            }
            let _ = server_wr.shutdown().await;
        })
    };

    let downstream = tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        let mut swallowed: Vec<u8> = Vec::new();
        loop {
            let n = match server_rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if dropping.load(Ordering::SeqCst) == 0 {
                if client_wr.write_all(&buf[..n]).await.is_err() {
                    break;
                }
                continue;
            }
            swallowed.extend_from_slice(&buf[..n]);
            if contains_ready_for_query(&swallowed) {
                // The command completed on the server; the client never
                // hears of it. Closing the socket is the ack loss.
                break;
            }
        }
        let _ = client_wr.shutdown().await;
    });

    let _ = tokio::join!(upstream, downstream);
}

/// The COMMIT command in a simple-query message: query strings are
/// NUL-terminated and bound parameters are length-prefixed, so `commit\0`
/// is the command and never a value such as `committed`.
fn find_commit(chunk: &[u8]) -> Option<usize> {
    chunk
        .windows(7)
        .position(|w| w[6] == 0 && w[..6].eq_ignore_ascii_case(b"commit"))
}

/// ReadyForQuery: `Z`, then a big-endian length of 5.
fn contains_ready_for_query(bytes: &[u8]) -> bool {
    bytes.windows(5).any(|w| w == [b'Z', 0, 0, 0, 5])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_is_found_as_a_command_only() {
        assert_eq!(find_commit(b"Q\0\0\0\x0bCOMMIT\0"), Some(5));
        assert_eq!(find_commit(b"Q\0\0\0\x0bcommit\0"), Some(5));
        assert_eq!(find_commit(b"committed\0"), None);
        assert_eq!(find_commit(b"select 'commit'\0"), None);
    }

    #[test]
    fn ready_for_query_is_recognised() {
        assert!(contains_ready_for_query(b"C\0\0\0\x0bCOMMIT\0Z\0\0\0\x05I"));
        assert!(!contains_ready_for_query(b"Z\0\0\0\x06I"));
    }
}
