//! `pool_tls` against a server that only accepts TLS.
//!
//! The server tests need H5I_APP_TEST_TLS_DATABASE_URL (host `localhost`,
//! a server certificate for `localhost` only, `hostnossl ... reject` in
//! `pg_hba.conf`), H5I_APP_TEST_TLS12_DATABASE_URL (the same, limited to
//! TLS 1.2) and H5I_APP_TEST_TLS_CA (the PEM CA that signed them);
//! `scripts/app/pg-tls.sh` starts both servers. The plaintext-server test
//! uses H5I_APP_TEST_DATABASE_URL, a server without TLS. Each skips when its
//! variables are missing.

use crate::tls::{server_end_point, Roots};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;
use crate::{pool, pool_tls, DbError, Pool};

fn env_of(var: &str) -> Option<(String, Roots)> {
    match (std::env::var(var), std::env::var("H5I_APP_TEST_TLS_CA")) {
        (Ok(url), Ok(ca)) => Some((url, Roots::PemFile(ca.into()))),
        _ => {
            eprintln!("{var} / H5I_APP_TEST_TLS_CA not set; skipping");
            None
        }
    }
}

fn env() -> Option<(String, Roots)> {
    env_of("H5I_APP_TEST_TLS_DATABASE_URL")
}

fn with_param(url: &str, param: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{param}")
}

/// An error with its causes, so a test can tell why a handshake failed.
fn chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(": ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

async fn ssl_of(p: &Pool) -> Result<(bool, String), DbError> {
    let c = p.0.get().await.map_err(|e| DbError::Pool(chain(&e)))?;
    let row = c.query_one("SELECT ssl, coalesce(version, '') FROM pg_stat_ssl WHERE pid = pg_backend_pid()", &[]).await?;
    Ok((row.get(0), row.get(1)))
}

#[tokio::test]
async fn verified_connection_is_encrypted() {
    let Some((url, roots)) = env() else { return };
    let p = pool_tls(&url, 2, &roots).unwrap();
    let (ssl, version) = ssl_of(&p).await.unwrap();
    assert!(ssl, "connection is not encrypted");
    assert!(version.starts_with("TLSv1."), "{version}");
}

#[tokio::test]
async fn sslmode_prefer_in_the_url_is_raised_to_require() {
    let Some((url, roots)) = env() else { return };
    let p = pool_tls(&with_param(&url, "sslmode=prefer"), 1, &roots).unwrap();
    assert!(ssl_of(&p).await.unwrap().0);
}

#[tokio::test]
async fn tls_1_2_server() {
    let Some((url, roots)) = env_of("H5I_APP_TEST_TLS12_DATABASE_URL") else { return };
    let p = pool_tls(&with_param(&url, "channel_binding=require"), 1, &roots).unwrap();
    assert_eq!(ssl_of(&p).await.unwrap(), (true, "TLSv1.2".to_string()));
}

/// The server checks the client's `tls-server-end-point` data against its own
/// certificate; with `require` there is no fallback to plain SCRAM.
#[tokio::test]
async fn channel_binding_is_accepted_by_the_server() {
    let Some((url, roots)) = env() else { return };
    let p = pool_tls(&with_param(&url, "channel_binding=require"), 1, &roots).unwrap();
    assert!(ssl_of(&p).await.unwrap().0);
}

#[tokio::test]
async fn server_without_tls_is_refused() {
    let Ok(url) = std::env::var("H5I_APP_TEST_DATABASE_URL") else {
        eprintln!("H5I_APP_TEST_DATABASE_URL not set; skipping");
        return;
    };
    let p = pool_tls(&url, 1, &Roots::WebPki).unwrap();
    let e = ssl_of(&p).await.unwrap_err().to_string();
    assert!(e.contains("server does not support TLS"), "{e}");
}

#[tokio::test]
async fn bundle_with_an_unusable_certificate_still_verifies() {
    let Some((url, _)) = env() else { return };
    let ca = std::fs::read_to_string(std::env::var("H5I_APP_TEST_TLS_CA").unwrap()).unwrap();
    // Well-formed PEM whose DER is not a certificate: webpki ignores it.
    let junk = "-----BEGIN CERTIFICATE-----\nMAMCAQA=\n-----END CERTIFICATE-----\n";
    let dir = std::env::temp_dir().join(format!("h5i-app-pg-bundle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bundle = dir.join("bundle.pem");
    std::fs::write(&bundle, format!("{junk}{ca}")).unwrap();
    let p = pool_tls(&url, 1, &Roots::PemFile(bundle.clone())).unwrap();
    assert!(ssl_of(&p).await.unwrap().0);
    // Only unusable certificates: refused when the pool is built.
    std::fs::write(&bundle, junk).unwrap();
    let e = pool_tls(&url, 1, &Roots::PemFile(bundle)).err().unwrap().to_string();
    assert!(e.contains("no usable certificates"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn certificate_from_an_untrusted_ca_is_refused() {
    let Some((url, _)) = env() else { return };
    let p = pool_tls(&url, 1, &Roots::WebPki).unwrap();
    let e = ssl_of(&p).await.unwrap_err().to_string();
    assert!(e.contains("UnknownIssuer"), "{e}");
}

#[tokio::test]
async fn host_name_not_in_the_certificate_is_refused() {
    let Some((url, roots)) = env() else { return };
    assert!(url.contains("@localhost"), "test URL must use host localhost");
    let p = pool_tls(&url.replace("@localhost", "@127.0.0.1"), 1, &roots).unwrap();
    let e = ssl_of(&p).await.unwrap_err().to_string();
    assert!(e.contains("not valid for name"), "{e}");
}

#[tokio::test]
async fn plain_pool_is_refused_by_a_tls_only_server() {
    let Some((url, _)) = env() else { return };
    let p = pool(&url, 1).unwrap();
    assert!(ssl_of(&p).await.is_err());
}

#[tokio::test]
async fn lockdown_connection_is_verified_and_encrypted() {
    let Some((url, roots)) = env() else { return };
    let (c, _conn) = crate::roles::connect_tls(&url, &roots).await.unwrap();
    let row = c.query_one("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()", &[]).await.unwrap();
    assert!(row.get::<_, bool>(0));
    match crate::roles::connect_tls(&url, &Roots::WebPki).await.err().unwrap() {
        DbError::Postgres(e) => assert!(chain(&e).contains("UnknownIssuer"), "{}", chain(&e)),
        e => panic!("{e}"),
    }
}

#[test]
fn sslmode_disable_is_an_error() {
    let e = pool_tls("postgres://u@localhost/db?sslmode=disable", 1, &Roots::WebPki).err().unwrap();
    assert!(e.to_string().contains("sslmode=disable"), "{e}");
}

#[test]
fn ca_file_without_certificates_is_an_error() {
    let dir = std::env::temp_dir().join(format!("h5i-app-pg-tls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let empty = dir.join("empty.pem");
    std::fs::write(&empty, "not a certificate\n").unwrap();
    assert!(pool_tls("postgres://u@localhost/db", 1, &Roots::PemFile(empty)).is_err());
    assert!(pool_tls("postgres://u@localhost/db", 1, &Roots::PemFile(dir.join("missing.pem"))).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Expected hashes from `openssl x509 -outform der | openssl dgst -<hash>`.
#[test]
fn server_end_point_follows_rfc_5929() {
    let cases: &[(&str, Option<&str>)] = &[
        (include_str!("tls_fixtures/rsa-sha1.pem"), Some("2505927d8fb256f801df1494799aaa904ef5bf7ea774224d79acc6be150eaad2")),
        (include_str!("tls_fixtures/rsa-sha256.pem"), Some("c464c049a04058494142c7c680fae1dd125fd4f90a75ae9f2f179e26a79d5e57")),
        (
            include_str!("tls_fixtures/rsa-sha384.pem"),
            Some("4026606e6e6932eab8a310f4ba9900a2d4630c529b3838d21d48dc0dfce460921351506fdf4e761fc3b54915da806455"),
        ),
        (
            include_str!("tls_fixtures/rsa-sha512.pem"),
            Some("94a7a2498952614db3e5f6e692c34e4845387f55d451e1af7f0cb2355ce0f0c77b0bee494a0de4c5aafcdfe0d33089ae34958ca4450bb995f19a12ef8335b288"),
        ),
        (include_str!("tls_fixtures/ecdsa-sha256.pem"), Some("cf6466f5abd0016771e5e437ad7e257abce6b379d3c93088b6163ce7bb8a802f")),
        (
            include_str!("tls_fixtures/ecdsa-sha384.pem"),
            Some("9152043673ff911e2c13de1ccec615fa33365421d8032bd9fc23d140639d5164bb0ec5c41d69e55433b3be28d04305a0"),
        ),
        (
            include_str!("tls_fixtures/ecdsa-sha512.pem"),
            Some("bf445468d1508fcc065d5856f6d42ac3c3712f599c4e34e3bed027b3b644fa379e8caf9164629ead53ceb579b11f93625977e4c33dbf1fa56ec27f1dd4ea9b99"),
        ),
        // No single hash for the signature: no binding data.
        (include_str!("tls_fixtures/ed25519.pem"), None),
        (include_str!("tls_fixtures/rsa-pss.pem"), None),
    ];
    for (pem, want) in cases {
        let der = CertificateDer::from_pem_slice(pem.as_bytes()).unwrap();
        assert_eq!(server_end_point(der.as_ref()).map(|h| hex(&h)).as_deref(), *want);
    }
    assert_eq!(server_end_point(&[]), None);
    assert_eq!(server_end_point(&[0x30, 0x82, 0xff]), None);
}
