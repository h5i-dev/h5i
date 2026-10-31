//! TLS to PostgreSQL with rustls (feature `tls`).
//!
//! [`pool_tls`](crate::pool_tls) and [`lockdown_tls`](crate::lockdown_tls)
//! connect with `sslmode=require` and a connector that verifies the server's
//! certificate chain against [`Roots`] and its name against the URL's host
//! (libpq's `verify-full`). There is no unverified mode and no fallback to
//! plain TCP. Channel binding is not offered; SCRAM still authenticates.

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::config::SslMode;
use tokio_postgres::tls::{ChannelBinding, MakeTlsConnect, TlsConnect};

use crate::DbError;

/// Trust anchors for the server's certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Roots {
    /// Mozilla's root store (`webpki-roots`), for a server with a publicly
    /// trusted certificate.
    WebPki,
    /// Only the CA certificates in this PEM file, e.g. the Amazon RDS bundle.
    PemFile(PathBuf),
}

/// A `tokio-postgres` connector that verifies the server (see the module docs).
#[derive(Clone)]
pub struct RustlsConnect(Arc<rustls::ClientConfig>);

impl RustlsConnect {
    pub fn new(roots: &Roots) -> Result<Self, DbError> {
        let mut store = rustls::RootCertStore::empty();
        match roots {
            Roots::WebPki => store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
            Roots::PemFile(path) => {
                let certs = CertificateDer::pem_file_iter(path)
                    .and_then(|it| it.collect::<Result<Vec<_>, _>>())
                    .map_err(|e| DbError::Pool(format!("CA file {}: {e}", path.display())))?;
                let (added, _) = store.add_parsable_certificates(certs);
                if added == 0 {
                    return Err(DbError::Pool(format!("CA file {}: no usable certificates", path.display())));
                }
            }
        }
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| DbError::Pool(format!("tls: {e}")))?
            .with_root_certificates(store)
            .with_no_client_auth();
        Ok(RustlsConnect(Arc::new(config)))
    }
}

/// The URL's own settings, with TLS required. `sslmode=disable` is refused
/// rather than overridden.
pub(crate) fn require_tls(url: &str) -> Result<(), DbError> {
    let cfg = tokio_postgres::Config::from_str(url)?;
    if cfg.get_ssl_mode() == SslMode::Disable {
        return Err(DbError::Pool("TLS requested but the URL says sslmode=disable".into()));
    }
    Ok(())
}

impl<S> MakeTlsConnect<S> for RustlsConnect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = TlsStream<S>;
    type TlsConnect = RustlsTlsConnect;
    type Error = io::Error;

    fn make_tls_connect(&mut self, domain: &str) -> io::Result<RustlsTlsConnect> {
        // `tokio-postgres` passes "" when the URL has no host name (only
        // `hostaddr`); fail in `connect` so its error explains that.
        let name = ServerName::try_from(domain.to_string()).ok();
        if name.is_none() && !domain.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("invalid server name {domain:?}")));
        }
        Ok(RustlsTlsConnect { config: self.0.clone(), name })
    }
}

pub struct RustlsTlsConnect {
    config: Arc<rustls::ClientConfig>,
    name: Option<ServerName<'static>>,
}

impl<S> TlsConnect<S> for RustlsTlsConnect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = TlsStream<S>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<TlsStream<S>>> + Send>>;

    fn connect(self, stream: S) -> Self::Future {
        Box::pin(async move {
            let name = self.name.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no server name to verify"))?;
            let s = tokio_rustls::TlsConnector::from(self.config).connect(name, stream).await?;
            Ok(TlsStream(s))
        })
    }
}

pub struct TlsStream<S>(tokio_rustls::client::TlsStream<S>);

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for TlsStream<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for TlsStream<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> tokio_postgres::tls::TlsStream for TlsStream<S> {
    fn channel_binding(&self) -> ChannelBinding {
        ChannelBinding::none()
    }
}
