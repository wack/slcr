//! A local HTTPS server for tests of installing descriptors, with a
//! certificate signed by a throwaway authority that a [slcr::Fetcher]
//! can be told to trust.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};

/// A running server, which answers every request with one body, which
/// can be changed. It stops when the runtime it was started on does.
pub(crate) struct HttpsServer {
    port: u16,
    /// The authority that signed its certificate, in PEM.
    authority: String,
    requests: Arc<AtomicUsize>,
    body: Arc<Mutex<String>>,
}

impl HttpsServer {
    /// Serve `body` at every path. Must be called within a tokio runtime.
    pub(crate) async fn start(body: &str) -> Self {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};

        let authority_key = KeyPair::generate().unwrap();
        let mut authority_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        authority_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let authority = authority_params.self_signed(&authority_key).unwrap();
        let pem = authority.pem();
        let issuer = Issuer::new(authority_params, authority_key);
        let server_key = KeyPair::generate().unwrap();
        let server = CertificateParams::new(vec!["127.0.0.1".to_owned()])
            .unwrap()
            .signed_by(&server_key, &issuer)
            .unwrap();
        let chain: Vec<CertificateDer<'static>> =
            vec![server.der().clone(), authority.der().clone()];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der()));
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(AtomicUsize::new(0));
        let body = Arc::new(Mutex::new(body.to_owned()));
        let counted = Arc::clone(&requests);
        let served = Arc::clone(&body);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let body = served.lock().unwrap().clone();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let counted = Arc::clone(&counted);
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    counted.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self {
            port,
            authority: pem,
            requests,
            body,
        }
    }

    /// Serve `body` from now on.
    pub(crate) fn serve(&self, body: &str) {
        *self.body.lock().unwrap() = body.to_owned();
    }

    /// The authority that signed its certificate, in PEM.
    pub(crate) fn authority(&self) -> &str {
        &self.authority
    }

    /// The `https://` URL of `path` on this server.
    pub(crate) fn url(&self, path: &str) -> String {
        format!("https://127.0.0.1:{}{path}", self.port)
    }

    /// A fetcher that trusts only this server's authority.
    pub(crate) fn fetcher(&self) -> slcr::Fetcher {
        slcr::Fetcher::trusting_only(self.authority.as_bytes()).unwrap()
    }

    /// How many requests it has answered.
    pub(crate) fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}
