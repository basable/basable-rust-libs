//! The wire between two replicas: mutual TLS 1.3, then a hello from the
//! side that dialled, then publish frames one way and acknowledgements the
//! other.
//!
//! ```text
//! hello   "BPS1" | replica id (16 bytes)
//! publish 0x01 | seq (u64) | channel length (u16) | channel | data length (u32) | data
//! ack     0x02 | seq (u64) | 0x01 delivered to a handler, 0x00 no handler on the channel
//! ```
//!
//! Integers are big-endian, as in the Go original (whose replicas never talk
//! to these: no process has a peer of the other language). Neither side
//! checks a certificate chain: each accepts only the certificate whose
//! SHA-256 the directory lists for the replica at the other end, and checks
//! the handshake signature against it, so the peer holds its key.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use basable_core::{BoxError, Ctx};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{
    CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use uuid::Uuid;

use crate::directory::{self, Target};
use crate::{Error, Inner, MAX_CHANNEL_BYTES, MAX_MESSAGE_BYTES, Message};

const HELLO_MAGIC: &[u8; 4] = b"BPS1";
const FRAME_PUBLISH: u8 = 1;
const FRAME_ACK: u8 = 2;
const ACK_DELIVERED: u8 = 1;
const ACK_NO_SUBSCRIBER: u8 = 0;

/// The name every client asks for; nothing checks it, the pin does the work.
const SERVER_NAME: &str = "pubsub";

fn tls_error(e: impl std::fmt::Display) -> Error {
    Error::Tls(e.to_string())
}

fn fingerprint_of(cert: &[u8]) -> [u8; 32] {
    Sha256::digest(cert).into()
}

/// The replica's key pair and self-signed certificate, made at start, and
/// the server configuration built on them. It lives as long as the process;
/// nothing checks its dates or its chain, only its fingerprint.
pub(crate) struct Identity {
    pub(crate) fingerprint: [u8; 32],
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    provider: Arc<CryptoProvider>,
    server: Arc<rustls::ServerConfig>,
}

impl Identity {
    pub(crate) fn new(id: Uuid) -> Result<Identity, Error> {
        let certified =
            rcgen::generate_simple_self_signed(vec![format!("pubsub-{id}")]).map_err(tls_error)?;
        let cert = certified.cert.der().clone();
        let key =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let server = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(tls_error)?
            .with_client_cert_verifier(Arc::new(AnyClientCertificate {
                algorithms: provider.signature_verification_algorithms,
            }))
            .with_single_cert(vec![cert.clone()], key.clone_key())
            .map_err(tls_error)?;
        Ok(Identity {
            fingerprint: fingerprint_of(&cert),
            cert,
            key,
            provider,
            server: Arc::new(server),
        })
    }

    /// The client configuration for one peer: this replica's certificate,
    /// and only the peer's listed one accepted.
    fn client(&self, fingerprint: [u8; 32]) -> Result<rustls::ClientConfig, Error> {
        rustls::ClientConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(tls_error)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pinned {
                fingerprint,
                algorithms: self.provider.signature_verification_algorithms,
            }))
            .with_client_auth_cert(vec![self.cert.clone()], self.key.clone_key())
            .map_err(tls_error)
    }
}

/// Accepts exactly the server certificate whose SHA-256 is `fingerprint`.
#[derive(Debug)]
struct Pinned {
    fingerprint: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint_of(end_entity) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "the peer's certificate is not the one the directory lists".to_string(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Requires a client certificate and checks its handshake signature; which
/// certificate it is, the server checks against the directory once the
/// hello names the replica it claims to be.
#[derive(Debug)]
struct AnyClientCertificate {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ClientCertVerifier for AnyClientCertificate {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

type ClientStream = tokio_rustls::client::TlsStream<TcpStream>;
type Waiting = Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>;

/// This replica's connection to another one, which it publishes over.
pub(crate) struct Peer {
    target: Target,
    /// Keeps frames whole and in publish order.
    writer: tokio::sync::Mutex<WriteHalf<ClientStream>>,
    waiting: Waiting,
    seq: AtomicU64,
    failed: Arc<AtomicBool>,
    reader: JoinHandle<()>,
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Peer {
    fn alive(&self) -> bool {
        !self.failed.load(Ordering::SeqCst)
    }

    /// Marks the connection dead; every publish waiting on it returns, and
    /// the next one dials afresh.
    fn fail(&self) {
        self.failed.store(true, Ordering::SeqCst);
        self.reader.abort();
        self.waiting.lock().expect("waiting lock").clear();
    }

    /// Writes one frame and waits for its acknowledgement. A peer that does
    /// not acknowledge in time is failed, so the next publish dials afresh
    /// rather than queueing behind a connection that may be dead.
    async fn publish(
        &self,
        channel: &str,
        data: &[u8],
        timeout: Duration,
    ) -> Result<bool, BoxError> {
        if !self.alive() {
            return Err("the connection failed".into());
        }
        let (ack, acked) = oneshot::channel();
        {
            let mut writer = self.writer.lock().await;
            let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
            self.waiting.lock().expect("waiting lock").insert(seq, ack);
            let header = publish_header(seq, channel, data.len());
            let write = async {
                writer.write_all(&header).await?;
                writer.write_all(data).await?;
                writer.flush().await
            };
            match tokio::time::timeout(timeout, write).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    self.fail();
                    return Err(e.into());
                }
                Err(_) => {
                    self.fail();
                    return Err(format!("the write did not finish within {timeout:?}").into());
                }
            }
        }
        match tokio::time::timeout(timeout, acked).await {
            Ok(Ok(taken)) => Ok(taken),
            Ok(Err(_)) => Err("the connection failed before the acknowledgement".into()),
            Err(_) => {
                self.fail();
                Err(format!("no acknowledgement within {timeout:?}").into())
            }
        }
    }
}

/// Delivers one message to `target` and reports whether a handler there
/// took it.
pub(crate) async fn send(
    inner: &Inner,
    target: &Target,
    channel: &str,
    data: &[u8],
) -> Result<bool, BoxError> {
    let peer = peer_for(inner, target).await?;
    peer.publish(channel, data, inner.cfg.send_timeout).await
}

/// The open connection to `target`, dialled on first use and again after it
/// failed.
async fn peer_for(inner: &Inner, target: &Target) -> Result<Arc<Peer>, BoxError> {
    {
        let mut peers = inner.peers.lock().await;
        if let Some(peer) = peers.get(&target.id) {
            if peer.alive() && peer.target == *target {
                return Ok(peer.clone());
            }
            peers.remove(&target.id);
        }
    }
    let dialled = Arc::new(dial(inner, target).await?);
    let mut peers = inner.peers.lock().await;
    if let Some(existing) = peers.get(&target.id)
        && existing.alive()
        && existing.target == *target
    {
        // Another publish dialled it meanwhile; keep one connection, so
        // messages keep their order.
        return Ok(existing.clone());
    }
    peers.insert(target.id, dialled.clone());
    Ok(dialled)
}

async fn dial(inner: &Inner, target: &Target) -> Result<Peer, BoxError> {
    let identity = inner.identity.as_ref().ok_or("a local bus has no peers")?;
    let timeout = inner.cfg.send_timeout;
    let connector = TlsConnector::from(Arc::new(identity.client(target.fingerprint)?));
    let tcp = tokio::time::timeout(timeout, TcpStream::connect(&target.address)).await??;
    tcp.set_nodelay(true)?;
    let name = ServerName::try_from(SERVER_NAME)?;
    let mut tls = tokio::time::timeout(timeout, connector.connect(name, tcp)).await??;

    let mut hello = Vec::with_capacity(HELLO_MAGIC.len() + 16);
    hello.extend_from_slice(HELLO_MAGIC);
    hello.extend_from_slice(inner.id.as_bytes());
    tokio::time::timeout(timeout, async {
        tls.write_all(&hello).await?;
        tls.flush().await
    })
    .await??;

    let (read, write) = tokio::io::split(tls);
    let waiting: Waiting = Arc::new(Mutex::new(HashMap::new()));
    let failed = Arc::new(AtomicBool::new(false));
    let reader = tokio::spawn(read_acks(read, waiting.clone(), failed.clone()));
    Ok(Peer {
        target: target.clone(),
        writer: tokio::sync::Mutex::new(write),
        waiting,
        seq: AtomicU64::new(0),
        failed,
        reader,
    })
}

async fn read_acks(mut read: ReadHalf<ClientStream>, waiting: Waiting, failed: Arc<AtomicBool>) {
    let mut frame = [0u8; 10];
    loop {
        if read.read_exact(&mut frame).await.is_err() || frame[0] != FRAME_ACK {
            break;
        }
        let seq = u64::from_be_bytes(frame[1..9].try_into().expect("eight bytes"));
        let ack = waiting.lock().expect("waiting lock").remove(&seq);
        if let Some(ack) = ack {
            let _ = ack.send(frame[9] == ACK_DELIVERED);
        }
    }
    failed.store(true, Ordering::SeqCst);
    waiting.lock().expect("waiting lock").clear();
}

/// Serves every connection a peer opens until `ctx` is cancelled.
pub(crate) async fn accept(inner: Arc<Inner>, listener: TcpListener, ctx: Ctx) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = ctx.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((tcp, remote)) => {
                    let inner = inner.clone();
                    let ctx = ctx.clone();
                    connections.spawn(async move { serve(inner, tcp, remote, ctx).await });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "pubsub: accept failed");
                    tokio::select! {
                        _ = ctx.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                    }
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    drop(listener);
    while connections.join_next().await.is_some() {}
}

/// Checks who dialled, then delivers each frame to this replica's handlers
/// and acknowledges it.
async fn serve(inner: Arc<Inner>, tcp: TcpStream, remote: SocketAddr, ctx: Ctx) {
    let Some(identity) = inner.identity.as_ref() else {
        return;
    };
    let timeout = inner.cfg.send_timeout;
    let acceptor = TlsAcceptor::from(identity.server.clone());
    let mut tls = match tokio::time::timeout(timeout, acceptor.accept(tcp)).await {
        Ok(Ok(tls)) => tls,
        Ok(Err(e)) => {
            tracing::debug!(%remote, error = %e, "pubsub: peer handshake failed");
            return;
        }
        Err(_) => return,
    };
    let mut hello = [0u8; 20];
    if !matches!(
        tokio::time::timeout(timeout, tls.read_exact(&mut hello)).await,
        Ok(Ok(_))
    ) {
        return;
    }
    if &hello[..HELLO_MAGIC.len()] != HELLO_MAGIC {
        tracing::warn!(%remote, "pubsub: refused a connection without the hello");
        return;
    }
    let id = Uuid::from_slice(&hello[HELLO_MAGIC.len()..]).expect("sixteen bytes");
    let presented = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
        .map(|cert| fingerprint_of(cert));
    let expected = match directory::live_fingerprint(&inner, id).await {
        Ok(expected) => expected,
        Err(e) => {
            tracing::warn!(replica = %id, error = %e, "pubsub: could not check a peer against the directory");
            return;
        }
    };
    if presented.is_none() || presented != expected {
        tracing::warn!(replica = %id, %remote, "pubsub: refused a peer the directory does not list");
        return;
    }

    let origin = id.to_string();
    let (read, mut write) = tokio::io::split(tls);
    let mut read = BufReader::with_capacity(64 << 10, read);
    loop {
        let frame = tokio::select! {
            _ = ctx.cancelled() => return,
            frame = read_publish(&mut read) => frame,
        };
        let (seq, channel, data) = match frame {
            Ok(frame) => frame,
            Err(e) => {
                tracing::debug!(replica = %id, error = %e, "pubsub: peer connection ended");
                return;
            }
        };
        let delivered = inner.deliver_local(&Message {
            channel,
            origin: origin.clone(),
            data,
        });
        let mut ack = [0u8; 10];
        ack[0] = FRAME_ACK;
        ack[1..9].copy_from_slice(&seq.to_be_bytes());
        ack[9] = if delivered {
            ACK_DELIVERED
        } else {
            ACK_NO_SUBSCRIBER
        };
        let wrote = tokio::time::timeout(timeout, async {
            write.write_all(&ack).await?;
            write.flush().await
        })
        .await;
        if !matches!(wrote, Ok(Ok(()))) {
            return;
        }
    }
}

fn publish_header(seq: u64, channel: &str, data_len: usize) -> Vec<u8> {
    let mut header = Vec::with_capacity(11 + channel.len() + 4);
    header.push(FRAME_PUBLISH);
    header.extend_from_slice(&seq.to_be_bytes());
    header.extend_from_slice(&(channel.len() as u16).to_be_bytes());
    header.extend_from_slice(channel.as_bytes());
    header.extend_from_slice(&(data_len as u32).to_be_bytes());
    header
}

async fn read_publish<R: AsyncRead + Unpin>(r: &mut R) -> Result<(u64, String, Vec<u8>), BoxError> {
    let mut head = [0u8; 11];
    r.read_exact(&mut head).await?;
    if head[0] != FRAME_PUBLISH {
        return Err(format!("unexpected frame type {}", head[0]).into());
    }
    let seq = u64::from_be_bytes(head[1..9].try_into().expect("eight bytes"));
    let channel_len = u16::from_be_bytes([head[9], head[10]]) as usize;
    if channel_len == 0 || channel_len > MAX_CHANNEL_BYTES {
        return Err(format!("channel length {channel_len} out of range").into());
    }
    let mut channel = vec![0u8; channel_len];
    r.read_exact(&mut channel).await?;
    let channel = String::from_utf8(channel)?;
    let mut size = [0u8; 4];
    r.read_exact(&mut size).await?;
    let data_len = u32::from_be_bytes(size) as usize;
    if data_len > MAX_MESSAGE_BYTES {
        return Err(format!("message length {data_len} over the limit").into());
    }
    let mut data = vec![0u8; data_len];
    r.read_exact(&mut data).await?;
    Ok((seq, channel, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u64, channel: &str, data: &[u8]) -> Vec<u8> {
        let mut wire = publish_header(seq, channel, data.len());
        wire.extend_from_slice(data);
        wire
    }

    #[tokio::test]
    async fn a_publish_frame_round_trips() {
        for size in [0, 1, 64 << 10, 3 << 20] {
            let data = vec![0xab; size];
            let wire = frame(42, "tenant_events", &data);
            let (seq, channel, got) = read_publish(&mut wire.as_slice()).await.unwrap();
            assert_eq!(
                (seq, channel.as_str(), got.len()),
                (42, "tenant_events", size)
            );
            assert_eq!(got, data);
        }
    }

    #[tokio::test]
    async fn bad_frames_are_refused() {
        let wire = frame(1, "c", b"x");
        let mut wrong_type = wire.clone();
        wrong_type[0] = FRAME_ACK;
        assert!(read_publish(&mut wrong_type.as_slice()).await.is_err());
        let mut no_channel = wire[..9].to_vec();
        no_channel.extend_from_slice(&[0, 0]);
        assert!(read_publish(&mut no_channel.as_slice()).await.is_err());
        let mut oversized = wire[..12].to_vec();
        oversized.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        assert!(read_publish(&mut oversized.as_slice()).await.is_err());
    }

    #[test]
    fn the_pin_accepts_only_the_listed_certificate() {
        let mine = Identity::new(Uuid::new_v4()).unwrap();
        let other = Identity::new(Uuid::new_v4()).unwrap();
        let pinned = Pinned {
            fingerprint: mine.fingerprint,
            algorithms: mine.provider.signature_verification_algorithms,
        };
        let name = ServerName::try_from(SERVER_NAME).unwrap();
        assert!(
            pinned
                .verify_server_cert(&mine.cert, &[], &name, &[], UnixTime::now())
                .is_ok()
        );
        assert!(
            pinned
                .verify_server_cert(&other.cert, &[], &name, &[], UnixTime::now())
                .is_err()
        );
    }
}
