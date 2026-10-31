//! `pool_tls` against a server that only accepts TLS.
//!
//! The server tests need H5I_APP_TEST_TLS_DATABASE_URL (host `localhost`,
//! a server certificate for `localhost` only, `hostnossl ... reject` in
//! `pg_hba.conf`) and H5I_APP_TEST_TLS_CA (the PEM CA that signed it); they
//! skip otherwise. `scripts/pg-tls-test.sh` starts such a server.

use crate::tls::Roots;
use crate::{pool, pool_tls, DbError, Pool};

fn env() -> Option<(String, Roots)> {
    match (std::env::var("H5I_APP_TEST_TLS_DATABASE_URL"), std::env::var("H5I_APP_TEST_TLS_CA")) {
        (Ok(url), Ok(ca)) => Some((url, Roots::PemFile(ca.into()))),
        _ => {
            eprintln!("H5I_APP_TEST_TLS_DATABASE_URL / H5I_APP_TEST_TLS_CA not set; skipping");
            None
        }
    }
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
    let sep = if url.contains('?') { '&' } else { '?' };
    let p = pool_tls(&format!("{url}{sep}sslmode=prefer"), 1, &roots).unwrap();
    assert!(ssl_of(&p).await.unwrap().0);
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
