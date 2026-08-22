//! US-23: TLS is actually served, not merely configured.
//!
//! The resolution matrix has unit tests, but nothing exercised `tls::serve` — so
//! "`--tls-cert` / `--tls-key` serve a supplied certificate" was a criterion with no
//! coverage at all. A wrong certificate loader would have been discovered by the first
//! person to deploy with one.
//!
//! ACME and the plain path are not covered here: ACME needs Let's Encrypt and a public
//! domain, which belongs with the manual verification step.

use std::net::SocketAddr;

use axum::{Router, routing::get};
use mdshelf::server::tls::{TlsMode, serve};

/// Write a throwaway self-signed certificate for `localhost`.
fn self_signed(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("generating a self-signed certificate");
    let cert_path = dir.join("dev.crt");
    let key_path = dir.join("dev.key");
    std::fs::write(&cert_path, certificate.cert.pem()).expect("writing cert");
    std::fs::write(&key_path, certificate.signing_key.serialize_pem()).expect("writing key");
    (cert_path, key_path)
}

/// A free port, released before the server binds it.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binding");
    listener.local_addr().expect("addr").port()
}

#[tokio::test]
async fn a_supplied_certificate_actually_serves_https() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (cert, key) = self_signed(dir.path());
    let port = free_port();
    let address: SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr");

    let router = Router::new().route("/", get(|| async { "hello over tls" }));
    tokio::spawn(async move {
        let _ = serve(TlsMode::Supplied { cert, key }, address, router).await;
    });

    // Give the listener a moment to bind.
    for _ in 0..40 {
        if std::net::TcpStream::connect(address).is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let client = reqwest::Client::builder()
        // The certificate is self-signed by construction; this test is about whether
        // TLS is served at all, not about trust chains.
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("client");

    let response = client
        .get(format!("https://localhost:{port}/"))
        .send()
        .await
        .expect("the server should complete a TLS handshake");
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.expect("body"), "hello over tls");
}

/// A plain-HTTP request to a TLS listener must fail rather than be served in the clear.
#[tokio::test]
async fn a_tls_listener_does_not_answer_plain_http() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (cert, key) = self_signed(dir.path());
    let port = free_port();
    let address: SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr");

    let router = Router::new().route("/", get(|| async { "hello over tls" }));
    tokio::spawn(async move {
        let _ = serve(TlsMode::Supplied { cert, key }, address, router).await;
    });
    for _ in 0..40 {
        if std::net::TcpStream::connect(address).is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Prove the server is actually up first. Before the crypto-provider fix this test
    // passed against a server that had panicked on startup — the plain-HTTP request
    // failed because nothing was listening, which is the right answer for the wrong
    // reason. Establishing that HTTPS works makes the negative result mean something.
    let tls_client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("client");
    let alive = tls_client
        .get(format!("https://localhost:{port}/"))
        .send()
        .await
        .expect("the TLS listener must be serving before this test means anything");
    assert_eq!(alive.status(), 200);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client");
    let result = client.get(format!("http://localhost:{port}/")).send().await;
    assert!(
        result.is_err(),
        "a TLS listener answered a plain-HTTP request: {result:?}"
    );
}

/// US-22: `auth setup --self-signed` must produce a certificate that actually works.
///
/// Closes the loop between the two features: the wizard writes the pair, and the TLS
/// path serves with it. A wizard that emits an unusable certificate is worse than no
/// wizard, because the failure surfaces later and somewhere else.
#[tokio::test]
async fn the_wizards_self_signed_certificate_serves_tls() {
    let dir = tempfile::tempdir().expect("temp dir");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mdshelf"))
        .args([
            "auth",
            "setup",
            "--self-signed",
            dir.path().to_str().expect("path"),
            "--public-url",
            "https://localhost",
        ])
        .output()
        .expect("running the wizard");
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The caveats matter as much as the files: this certificate is not a deployment.
    assert!(stdout.contains("Browsers will warn"), "got:\n{stdout}");
    assert!(
        stdout.contains("Google will not accept a self-signed host"),
        "got:\n{stdout}"
    );

    let cert = dir.path().join("mdshelf-dev.crt");
    let key = dir.path().join("mdshelf-dev.key");
    assert!(cert.is_file() && key.is_file());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key)
            .expect("key metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the private key must be owner-only");
    }

    // And now the point: it has to serve.
    let port = free_port();
    let address: SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr");
    let router = Router::new().route("/", get(|| async { "wizard cert works" }));
    tokio::spawn(async move {
        let _ = serve(TlsMode::Supplied { cert, key }, address, router).await;
    });
    for _ in 0..40 {
        if std::net::TcpStream::connect(address).is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("client");
    let response = client
        .get(format!("https://localhost:{port}/"))
        .send()
        .await
        .expect("the wizard's certificate should complete a handshake");
    assert_eq!(response.text().await.expect("body"), "wizard cert works");
}

/// A missing or unreadable certificate must fail at startup with a clear message,
/// not at the first request.
#[tokio::test]
async fn a_bad_certificate_fails_before_serving() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cert = dir.path().join("not-a-cert.crt");
    let key = dir.path().join("not-a-key.key");
    std::fs::write(&cert, "this is not a certificate").expect("write");
    std::fs::write(&key, "this is not a key").expect("write");

    let address: SocketAddr = format!("127.0.0.1:{}", free_port()).parse().expect("addr");
    let router = Router::new().route("/", get(|| async { "unreachable" }));

    let error = serve(TlsMode::Supplied { cert, key }, address, router)
        .await
        .expect_err("garbage PEM must not produce a running server");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("TLS certificate"),
        "the error should name what failed; got: {rendered}"
    );
}
