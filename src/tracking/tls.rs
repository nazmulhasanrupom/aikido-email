//! The public HTTPS listener of the tracking domains. One certificate per domain, picked by the name the client
//! asks for (SNI). The certificates are read from the database and kept in memory; they are read again when a
//! new one is saved and once a minute (so a certificate made by another server copy is used too).
use crate::state::AppState;
use rustls::sign::CertifiedKey;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;

#[derive(Default)]
pub struct CertStore {
    by_host: RwLock<HashMap<String, Arc<CertifiedKey>>>,
}

impl std::fmt::Debug for CertStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertStore").field("hosts", &self.hosts()).finish()
    }
}

/// A certificate chain and a key (both PEM) as a rustls key.
pub fn certified_key(chain_pem: &str, key_pem: &str) -> Result<CertifiedKey, String> {
    let chain: Vec<rustls_pki_types::CertificateDer<'static>> = pem::parse_many(chain_pem)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|p| p.tag() == "CERTIFICATE")
        .map(|p| rustls_pki_types::CertificateDer::from(p.into_contents()))
        .collect();
    if chain.is_empty() {
        return Err("no certificate in the chain".into());
    }
    let key = pem::parse(key_pem).map_err(|e| e.to_string())?;
    let der = rustls_pki_types::PrivateKeyDer::Pkcs8(rustls_pki_types::PrivatePkcs8KeyDer::from(key.into_contents()));
    let signing = rustls::crypto::ring::sign::any_supported_type(&der).map_err(|e| e.to_string())?;
    Ok(CertifiedKey::new(chain, signing))
}

impl CertStore {
    pub fn hosts(&self) -> Vec<String> {
        let mut h: Vec<String> = self.by_host.read().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        h.sort();
        h
    }

    pub fn set(&self, host: &str, key: CertifiedKey) {
        if let Ok(mut m) = self.by_host.write() {
            m.insert(host.to_ascii_lowercase(), Arc::new(key));
        }
    }

    /// Read every saved certificate. A damaged one is skipped with a warning.
    pub async fn refresh(&self, st: &AppState) -> Result<usize, sqlx::Error> {
        let rows: Vec<(uuid::Uuid, String, String, Vec<u8>, Vec<u8>)> =
            sqlx::query_as("SELECT id, subdomain, sending_domain, cert, cert_key FROM tracking_domains WHERE cert IS NOT NULL AND cert_key IS NOT NULL").fetch_all(&st.pool).await?;
        let mut fresh: HashMap<String, Arc<CertifiedKey>> = HashMap::new();
        for (id, sub, domain, cert, key) in rows {
            let host = format!("{sub}.{domain}");
            let parsed = super::open(&st.crypto, &cert, id.as_bytes())
                .and_then(|c| super::open(&st.crypto, &key, id.as_bytes()).map(|k| (c, k)))
                .and_then(|(c, k)| certified_key(&String::from_utf8_lossy(&c), &String::from_utf8_lossy(&k)));
            match parsed {
                Ok(k) => {
                    fresh.insert(host, Arc::new(k));
                }
                Err(e) => tracing::warn!(%host, error = %e, "a saved tracking certificate could not be read"),
            }
        }
        let n = fresh.len();
        if let Ok(mut m) = self.by_host.write() {
            *m = fresh;
        }
        Ok(n)
    }
}

impl rustls::server::ResolvesServerCert for CertStore {
    fn resolve(&self, hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let name = hello.server_name()?.trim_end_matches('.').to_ascii_lowercase();
        self.by_host.read().ok()?.get(&name).cloned()
    }
}

/// The address of the client of a TLS connection.
#[derive(Clone, Copy, Debug)]
pub struct Peer(pub SocketAddr);

type Accepted = (tokio_rustls::server::TlsStream<TcpStream>, Peer);

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, TlsListener>> for Peer {
    fn connect_info(stream: axum::serve::IncomingStream<'_, TlsListener>) -> Self {
        *stream.remote_addr()
    }
}

/// The routes read `ConnectInfo<SocketAddr>` (as on the plain listener): copy it from the TLS peer.
async fn peer_as_socket_addr(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    if let Some(axum::extract::ConnectInfo(p)) = req.extensions().get::<axum::extract::ConnectInfo<Peer>>().cloned() {
        req.extensions_mut().insert(axum::extract::ConnectInfo(p.0));
    }
    next.run(req).await
}

/// A listener for `axum::serve` that does the TLS handshake. Handshakes run in their own tasks, so a slow client
/// does not hold up the others.
pub struct TlsListener {
    rx: tokio::sync::mpsc::Receiver<Accepted>,
    local: SocketAddr,
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<TcpStream>;
    type Addr = Peer;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.rx.recv().await {
                Some(a) => return a,
                // the accept task ended: nothing more will come
                None => std::future::pending::<()>().await,
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(Peer(self.local))
    }
}

pub fn server_config(store: Arc<CertStore>) -> anyhow::Result<rustls::ServerConfig> {
    let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(store);
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

/// Serve the tracking routes over HTTPS on `addr`. Runs until `cancel` is set.
pub async fn serve_https(st: AppState, addr: String, cancel: tokio_util::sync::CancellationToken) -> anyhow::Result<()> {
    let tcp = tokio::net::TcpListener::bind(&addr).await?;
    let local = tcp.local_addr()?;
    let acceptor = TlsAcceptor::from(Arc::new(server_config(st.certs.clone())?));
    let (tx, rx) = tokio::sync::mpsc::channel::<Accepted>(256);
    let accept_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            let (sock, peer) = tokio::select! {
                _ = accept_cancel.cancelled() => return,
                r = tcp.accept() => match r {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::warn!(error = %e, "tracking listener: accept failed");
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        continue;
                    }
                },
            };
            let (acceptor, tx) = (acceptor.clone(), tx.clone());
            tokio::spawn(async move {
                let _ = sock.set_nodelay(true);
                // a client with no certificate for its name, or a slow one, is dropped here
                if let Ok(Ok(tls)) = tokio::time::timeout(std::time::Duration::from_secs(10), acceptor.accept(sock)).await {
                    let _ = tx.send((tls, Peer(peer))).await;
                }
            });
        }
    });
    tracing::info!(%local, "tracking HTTPS listener started");
    let app = super::server::public_app(st).layer(axum::middleware::from_fn(peer_as_socket_addr));
    axum::serve(TlsListener { rx, local }, app.into_make_service_with_connect_info::<Peer>())
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await?;
    Ok(())
}

/// Serve the tracking routes over plain HTTP on `addr` (the Let's Encrypt check comes here).
pub async fn serve_http(st: AppState, addr: String, cancel: tokio_util::sync::CancellationToken) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "tracking HTTP listener started");
    axum::serve(listener, super::server::public_app(st).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await?;
    Ok(())
}

/// Read the saved certificates again every minute (a certificate made by another server copy).
pub async fn refresh_loop(st: AppState, cancel: tokio_util::sync::CancellationToken) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
        }
        if let Err(e) = st.certs.refresh(&st).await {
            tracing::warn!(error = %format!("{e:#}"), "could not read the tracking certificates");
        }
    }
}
