//! TLS to PostgreSQL with rustls (feature `tls`).
//!
//! [`pool_tls`](crate::pool_tls) and [`lockdown_tls`](crate::lockdown_tls)
//! connect with `sslmode=require` and a connector that verifies the server's
//! certificate chain against [`Roots`] and its name against the URL's host
//! (libpq's `verify-full`). There is no unverified mode and no fallback to
//! plain TCP.
//!
//! SCRAM channel binding (`SCRAM-SHA-256-PLUS`, `tls-server-end-point` from
//! RFC 5929) is offered when the server certificate is signed with RSA
//! PKCS#1 v1.5 or ECDSA over MD5, SHA-1, SHA-256, SHA-384 or SHA-512. For
//! other signatures (RSA-PSS, Ed25519, SHA-224) no binding data is offered:
//! authentication falls back to plain SCRAM-SHA-256, and a URL with
//! `channel_binding=require` fails to connect.

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
        let cert = self.0.get_ref().1.peer_certificates().and_then(|c| c.first());
        match cert.and_then(|c| server_end_point(c.as_ref())) {
            Some(hash) => ChannelBinding::tls_server_end_point(hash),
            None => ChannelBinding::none(),
        }
    }
}

/// `tls-server-end-point` (RFC 5929 §4.1): the certificate's hash with its
/// signature's hash function, SHA-256 for MD5 and SHA-1. `None` when the
/// signature algorithm names no single supported hash.
pub(crate) fn server_end_point(der: &[u8]) -> Option<Vec<u8>> {
    let alg = match signature_oid(der)? {
        // md5WithRSAEncryption, sha1WithRSAEncryption, ecdsa-with-SHA1
        [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x04 | 0x05] | [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x01] => {
            &ring::digest::SHA256
        }
        // sha256WithRSAEncryption, ecdsa-with-SHA256
        [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b] | [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02] => {
            &ring::digest::SHA256
        }
        // sha384WithRSAEncryption, ecdsa-with-SHA384
        [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0c] | [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03] => {
            &ring::digest::SHA384
        }
        // sha512WithRSAEncryption, ecdsa-with-SHA512
        [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0d] | [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x04] => {
            &ring::digest::SHA512
        }
        _ => return None,
    };
    Some(ring::digest::digest(alg, der).as_ref().to_vec())
}

/// The OID of `Certificate.signatureAlgorithm` (RFC 5280 §4.1).
fn signature_oid(der: &[u8]) -> Option<&[u8]> {
    let (cert, _) = tlv(der, 0x30)?;
    let (_tbs, rest) = tlv(cert, 0x30)?;
    let (alg, _) = tlv(rest, 0x30)?;
    let (oid, _) = tlv(alg, 0x06)?;
    Some(oid)
}

/// One DER element with tag `tag`: its contents and what follows it.
fn tlv(b: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (&t, b) = b.split_first()?;
    if t != tag {
        return None;
    }
    let (&l, mut b) = b.split_first()?;
    let len = if l < 0x80 {
        l as usize
    } else {
        let n = (l & 0x7f) as usize;
        if n == 0 || n > 4 || b.len() < n {
            return None;
        }
        let len = b[..n].iter().fold(0usize, |a, &x| (a << 8) | x as usize);
        b = &b[n..];
        len
    };
    (b.len() >= len).then(|| b.split_at(len))
}
