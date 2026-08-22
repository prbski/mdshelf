//! Invariant 6: no secret reaches a log line, at any level.
//!
//! The design tries to make this structurally true — `Credentials` and `SecretKey` have
//! no `Debug`, token-endpoint errors are reduced to their OAuth code, and session ids
//! are redacted to a prefix. This test checks that the structure actually holds when the
//! code runs, including on the failure paths, which are where careless logging usually
//! creeps in.
//!
//! It lives in its own file because it installs a global tracing subscriber, and each
//! integration test file is a separate process.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use mdshelf::auth::IDLE_REFRESH_AFTER;
use mdshelf::auth::store::now_ms;
use mdshelf::auth::{SESSION_COOKIE, SessionOutcome};
use mdshelf::test_support::{MockIdp, TestServer, TokenBehaviour, TokenSpec, client};

/// The secret the harness configures mdshelf with.
const CLIENT_SECRET: &str = "test-client-secret";
/// The refresh token the mock issuer hands out.
const REFRESH_TOKEN: &str = "mock-refresh-token";

static CAPTURED: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn install_capture() -> Arc<Mutex<Vec<u8>>> {
    let buffer = CAPTURED
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .clone();
    // Everything, including TRACE: a secret logged at a level nobody normally enables
    // is still a secret written to disk by whoever does enable it.
    let _ = tracing_subscriber::fmt()
        .with_writer(CaptureWriter(buffer.clone()))
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .try_init();
    buffer
}

fn captured_text(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(
        &buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
    .into_owned()
}

fn query_params(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .expect("a parseable URL")
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn location_of(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("Location")
        .to_str()
        .expect("printable Location")
        .to_string()
}

const VAULT: &[(&str, &str)] = &[
    (
        "index.md",
        "---\ntitle: Home\nallow:\n  - ana@corp.com\n---\n\n# Home\n",
    ),
    // A malformed block, so the ERROR path that reports invalid rules also runs.
    (
        "broken.md",
        "---\ntitle: Broken\nallow: not-a-list\n---\n\n# Broken\n",
    ),
];

#[tokio::test]
async fn no_secret_appears_in_any_log_line() {
    let buffer = install_capture();

    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let runtime = server.state.auth.as_ref().expect("auth enabled");
    let http = client();

    // 1. A complete, successful sign-in: token exchange, ID-token verification, and
    //    session creation all handle secrets.
    let login = http
        .get(server.url("/auth/login"))
        .send()
        .await
        .expect("login");
    let authorize_url = location_of(&login);
    let state = query_params(&authorize_url)
        .get("state")
        .expect("state")
        .clone();
    idp.register_code(
        &format!("code-for-{state}"),
        TokenSpec::valid("ana@corp.com"),
    );
    let authorize = http.get(&authorize_url).send().await.expect("authorize");
    let callback = http
        .get(location_of(&authorize))
        .send()
        .await
        .expect("callback");
    let cookie = callback
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|header| {
            let rest = header.strip_prefix(&format!("{SESSION_COOKIE}="))?;
            Some(rest.split(';').next()?.to_string())
        })
        .expect("a session cookie");

    // 2. Serve some pages, including one denied by the malformed block.
    for path in ["/docs", "/docs/broken", "/docs/nope"] {
        let _ = http
            .get(server.url(path))
            .header(
                reqwest::header::COOKIE,
                format!("{SESSION_COOKIE}={cookie}"),
            )
            .send()
            .await;
    }

    // 3. A failed sign-in: the token endpoint refuses, exercising the error path that
    //    handles a response body which can echo request parameters.
    idp.set_behaviour(TokenBehaviour::InvalidGrant);
    let login = http
        .get(server.url("/auth/login"))
        .send()
        .await
        .expect("login");
    let authorize_url = location_of(&login);
    let state = query_params(&authorize_url)
        .get("state")
        .expect("state")
        .clone();
    idp.register_code(
        &format!("code-for-{state}"),
        TokenSpec::valid("bob@corp.com"),
    );
    let authorize = http.get(&authorize_url).send().await.expect("authorize");
    let _ = http.get(location_of(&authorize)).send().await;

    // 4. A revalidation failure, which logs the session, the address, and the cause.
    let now = now_ms();
    runtime
        .store
        .backdate_session(
            &cookie,
            now,
            now - IDLE_REFRESH_AFTER.as_millis() as i64 - 1_000,
        )
        .expect("backdating");
    assert!(matches!(
        runtime.resolve_session(&cookie).await,
        SessionOutcome::Anonymous
    ));

    // 5. And an unreachable provider, the other failure branch.
    idp.set_behaviour(TokenBehaviour::ServerError);
    let second = runtime
        .create_session("ana@corp.com", Some(REFRESH_TOKEN))
        .expect("session");
    runtime
        .store
        .backdate_session(
            &second,
            now,
            now - IDLE_REFRESH_AFTER.as_millis() as i64 - 1_000,
        )
        .expect("backdating");
    assert!(matches!(
        runtime.resolve_session(&second).await,
        SessionOutcome::Anonymous
    ));

    let logs = captured_text(&buffer);

    // The test must not pass simply because nothing was logged.
    assert!(
        logs.contains("session invalidated"),
        "expected the revalidation failure to be logged; captured:\n{logs}"
    );
    assert!(
        logs.contains("invalid access rule"),
        "expected the malformed rule block to be logged; captured:\n{logs}"
    );

    // Now the actual invariant.
    for (label, secret) in [
        ("the OAuth client secret", CLIENT_SECRET),
        ("a refresh token", REFRESH_TOKEN),
        ("a full session id", cookie.as_str()),
        ("a full session id", second.as_str()),
    ] {
        assert!(
            !logs.contains(secret),
            "{label} reached a log line.\n\
             Searched for: {secret}\n\
             ---- captured logs ----\n{logs}"
        );
    }

    // A session id may appear only as a short, non-replayable prefix.
    let prefix: String = cookie.chars().take(6).collect();
    if logs.contains(&prefix) {
        assert!(
            logs.contains(&format!("{prefix}…")),
            "a session id prefix appeared without the redaction marker"
        );
    }
}
