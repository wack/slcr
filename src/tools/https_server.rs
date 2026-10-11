//! A local HTTPS server for tests of fetching descriptors.
//!
//! It serves canned responses on `127.0.0.1` with a certificate signed by a
//! throwaway authority, and hands out a [Fetcher] that trusts only that
//! authority. The fetcher is otherwise the production one, so tests see
//! its real redirect, status, and size handling.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};

use super::source::{Fetcher, HttpsUrl};

/// What the server answers a request for one path with.
#[derive(Clone, Debug)]
pub(crate) struct Reply {
    status: &'static str,
    /// Where a redirect goes: a scheme and a path on this server.
    location: Option<(&'static str, &'static str)>,
    body: Vec<u8>,
}

impl Reply {
    /// `200 OK` with `body`.
    pub(crate) fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: "200 OK",
            location: None,
            body: body.into(),
        }
    }

    /// `404 Not Found`.
    pub(crate) fn not_found() -> Self {
        Self {
            status: "404 Not Found",
            location: None,
            body: Vec::new(),
        }
    }

    /// `302 Found`, redirecting to `path` on this server over HTTPS.
    pub(crate) fn redirect(path: &'static str) -> Self {
        Self::redirect_over("https", path)
    }

    /// `302 Found`, redirecting to `path` on this server over plain HTTP.
    pub(crate) fn redirect_to_http(path: &'static str) -> Self {
        Self::redirect_over("http", path)
    }

    fn redirect_over(scheme: &'static str, path: &'static str) -> Self {
        Self {
            status: "302 Found",
            location: Some((scheme, path)),
            body: Vec::new(),
        }
    }

    /// The response, from the server listening on `port`.
    fn to_bytes(&self, port: u16) -> Vec<u8> {
        let mut head = format!(
            "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.status,
            self.body.len()
        );
        if let Some((scheme, path)) = self.location {
            head.push_str(&format!("Location: {scheme}://127.0.0.1:{port}{path}\r\n"));
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

/// A running server. It stops when the runtime it was started on does.
pub(crate) struct HttpsServer {
    port: u16,
    authority: CertificateDer<'static>,
}

impl HttpsServer {
    /// Serve `routes`, pairs of a path such as `/jev.toml` and its reply.
    /// Any other path is `404 Not Found`. Must be called from within a
    /// `tokio` runtime.
    pub(crate) async fn start(routes: Vec<(&'static str, Reply)>) -> Self {
        let (authority, chain, key) = certificates();
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .expect("a valid certificate and key");
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes = Arc::new(routes);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                let routes = Arc::clone(&routes);
                tokio::spawn(async move {
                    // A client that rejects the certificate or the
                    // redirect hangs up; that is the test's business.
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let Some(path) = read_request_path(&mut stream).await else {
                        return;
                    };
                    let reply = routes
                        .iter()
                        .find(|(route, _)| *route == path)
                        .map_or_else(Reply::not_found, |(_, reply)| reply.clone());
                    let _ = stream.write_all(&reply.to_bytes(port)).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { port, authority }
    }

    /// The `https://` URL of `path` on this server.
    pub(crate) fn url(&self, path: &str) -> HttpsUrl {
        HttpsUrl::try_from(self.https(path)).unwrap()
    }

    /// The `https://` URL of `path`, as a string.
    pub(crate) fn https(&self, path: &str) -> String {
        format!("https://127.0.0.1:{}{path}", self.port)
    }

    /// A production [Fetcher] that trusts only this server's authority.
    pub(crate) fn fetcher(&self) -> Fetcher {
        let authority = reqwest::Certificate::from_der(&self.authority).unwrap();
        Fetcher::trusting(authority)
    }
}

/// The path in the request line of the request on `stream`, read up to
/// the end of its headers.
async fn read_request_path<S: AsyncReadExt + Unpin>(stream: &mut S) -> Option<String> {
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let request = String::from_utf8(request).ok()?;
    // `GET /path HTTP/1.1`
    request.split_whitespace().nth(1).map(str::to_owned)
}

/// A throwaway certificate authority, and a chain and key for `127.0.0.1`
/// that it signed.
fn certificates() -> (
    CertificateDer<'static>,
    Vec<CertificateDer<'static>>,
    PrivateKeyDer<'static>,
) {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};

    let authority_key = KeyPair::generate().unwrap();
    let mut authority_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    authority_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let authority = authority_params.self_signed(&authority_key).unwrap();
    let issuer = Issuer::new(authority_params, authority_key);

    let server_key = KeyPair::generate().unwrap();
    let server = CertificateParams::new(vec!["127.0.0.1".to_owned()])
        .unwrap()
        .signed_by(&server_key, &issuer)
        .unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der()));
    (
        authority.der().clone(),
        vec![server.der().clone(), authority.der().clone()],
        key,
    )
}
