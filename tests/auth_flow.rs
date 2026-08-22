//! Phase 1 — identity and session foundation (US-3, US-4, US-7, US-8).
//!
//! Everything here runs against a real mdshelf server and a real (local) OIDC issuer.
//! No part of the OAuth handshake is stubbed out, so a regression in state handling,
//! PKCE, or token verification fails these tests rather than reaching production.

use std::collections::HashMap;

use mdshelf::auth::store::now_ms;
use mdshelf::auth::{IDLE_REFRESH_AFTER, SESSION_COOKIE, SessionOutcome};
use mdshelf::test_support::{
    MockIdp, TEST_CLIENT_ID, TestServer, TokenBehaviour, TokenSpec, client,
};

const VAULT: &[(&str, &str)] = &[
    ("index.md", "---\ntitle: Home\n---\n\n# Home\n"),
    ("guide.md", "---\ntitle: Guide\n---\n\n# Guide\n"),
];

/// Parse the query parameters out of a URL.
fn query_params(url: &str) -> HashMap<String, String> {
    let parsed = url::Url::parse(url).expect("a parseable URL");
    parsed
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn location_of(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("a Location header")
        .to_str()
        .expect("a printable Location")
        .to_string()
}

fn set_cookie_headers(response: &reqwest::Response) -> Vec<String> {
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_string)
        .collect()
}

fn session_cookie_value(response: &reqwest::Response) -> Option<String> {
    set_cookie_headers(response).into_iter().find_map(|header| {
        let prefix = format!("{SESSION_COOKIE}=");
        let rest = header.strip_prefix(&prefix)?;
        let value = rest.split(';').next()?.to_string();
        (!value.is_empty()).then_some(value)
    })
}

/// Drive the full browser flow: login redirect, provider authorize, callback.
/// Returns the callback response so a test can assert on cookies and redirects.
async fn run_sign_in(
    server: &TestServer,
    idp: &MockIdp,
    next: &str,
    spec_for: impl Fn(&str) -> TokenSpec,
) -> reqwest::Response {
    let http = client();

    let login = http
        .get(server.url(&format!("/auth/login?next={next}")))
        .send()
        .await
        .expect("login request");
    assert_eq!(login.status(), 303, "login must redirect to the provider");

    let authorize_url = location_of(&login);
    let params = query_params(&authorize_url);
    let state = params.get("state").expect("state parameter").clone();

    // Register what the provider should mint when this code is redeemed.
    idp.register_code(&format!("code-for-{state}"), spec_for(&state));

    let authorize = http
        .get(&authorize_url)
        .send()
        .await
        .expect("provider authorize request");
    assert_eq!(authorize.status(), 303, "provider must redirect back");

    let callback_url = location_of(&authorize);
    http.get(&callback_url)
        .send()
        .await
        .expect("callback request")
}

/// Sign in successfully and return the session cookie value.
async fn sign_in(server: &TestServer, idp: &MockIdp, email: &str) -> String {
    let response = run_sign_in(server, idp, "/docs/guide", |_| TokenSpec::valid(email)).await;
    assert_eq!(response.status(), 303, "a verified sign-in must redirect");
    session_cookie_value(&response).expect("a session cookie")
}

// ---------------------------------------------------------------------------
// US-3: login route and provider redirect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_redirect_carries_every_required_parameter() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    let response = client()
        .get(server.url("/auth/login"))
        .send()
        .await
        .expect("login request");
    assert_eq!(response.status(), 303);

    let params = query_params(&location_of(&response));
    assert_eq!(
        params.get("client_id").map(String::as_str),
        Some(TEST_CLIENT_ID)
    );
    assert_eq!(
        params.get("response_type").map(String::as_str),
        Some("code")
    );
    assert_eq!(
        params.get("scope").map(String::as_str),
        Some("openid email")
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some(format!("{}/auth/callback", server.base_url).as_str())
    );
    assert!(params.contains_key("state"), "state is required (SEC-3)");
    assert!(params.contains_key("nonce"), "nonce is required (SEC-2)");
    assert!(
        params.contains_key("code_challenge"),
        "PKCE challenge is required"
    );
    // Without offline access Google only returns a refresh token on first consent,
    // which would leave most sessions unable to re-validate (D18).
    assert_eq!(
        params.get("access_type").map(String::as_str),
        Some("offline")
    );
}

#[tokio::test]
async fn login_state_and_nonce_differ_between_requests() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let http = client();

    let first = query_params(&location_of(
        &http.get(server.url("/auth/login")).send().await.unwrap(),
    ));
    let second = query_params(&location_of(
        &http.get(server.url("/auth/login")).send().await.unwrap(),
    ));

    assert_ne!(first.get("state"), second.get("state"));
    assert_ne!(first.get("nonce"), second.get("nonce"));
    assert_ne!(first.get("code_challenge"), second.get("code_challenge"));
}

#[tokio::test]
async fn login_rejects_off_site_next_parameters() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let http = client();

    // SEC-4: an open redirect after sign-in would let a phishing link borrow the
    // site's own domain for the hop.
    for hostile in [
        "https://evil.example.com",
        "//evil.example.com",
        "/\\evil.example.com",
    ] {
        let encoded = urlencoding_encode(hostile);
        let response = http
            .get(server.url(&format!("/auth/login?next={encoded}")))
            .send()
            .await
            .expect("login request");
        assert_eq!(
            response.status(),
            400,
            "next={hostile} must be refused, not silently rewritten"
        );
    }
}

fn urlencoding_encode(raw: &str) -> String {
    url::form_urlencoded::byte_serialize(raw.as_bytes()).collect()
}

// ---------------------------------------------------------------------------
// US-4: callback, token exchange, ID-token verification
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_login_flow_creates_a_session_and_returns_to_next() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    let response = run_sign_in(&server, &idp, "/docs/guide", |_| {
        TokenSpec::valid("ana@corp.com")
    })
    .await;

    assert_eq!(response.status(), 303);
    assert_eq!(
        location_of(&response),
        "/docs/guide",
        "the visitor must land where they were going"
    );

    let cookie = session_cookie_value(&response).expect("a session cookie");
    let runtime = server.state.auth.as_ref().expect("auth enabled");
    match runtime.resolve_session(&cookie).await {
        SessionOutcome::Active(email) => assert_eq!(email, "ana@corp.com"),
        SessionOutcome::Anonymous => panic!("a verified sign-in must yield a live session"),
    }
}

#[tokio::test]
async fn session_cookie_is_http_only_and_lax() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    let response = run_sign_in(&server, &idp, "/docs/guide", |_| {
        TokenSpec::valid("ana@corp.com")
    })
    .await;

    let header = set_cookie_headers(&response)
        .into_iter()
        .find(|header| header.starts_with(SESSION_COOKIE))
        .expect("a session cookie header");

    assert!(header.contains("HttpOnly"), "SEC-5: got {header}");
    assert!(header.contains("SameSite=Lax"), "SEC-5: got {header}");
    assert!(header.contains("Path=/"), "got {header}");
    // The harness serves over plain HTTP on loopback, where Secure would make the
    // cookie unusable and break local development.
    assert!(
        !header.contains("Secure"),
        "Secure must be omitted on a loopback origin: {header}"
    );
}

#[tokio::test]
async fn session_cookie_does_not_leak_the_email_or_a_token() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let cookie = sign_in(&server, &idp, "ana@corp.com").await;

    assert!(!cookie.contains("ana"), "the cookie must be opaque");
    assert!(!cookie.contains('@'));
    assert!(!cookie.contains("mock-refresh-token"));
}

/// Every way a token can be wrong must be rejected, and none may create a session.
#[tokio::test]
async fn forged_and_malformed_id_tokens_are_all_rejected() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let runtime = server.state.auth.as_ref().expect("auth enabled");

    /// A named way of corrupting the token minted for a flow.
    type Case = (&'static str, Box<dyn Fn(&str) -> TokenSpec>);

    let cases: Vec<Case> = vec![
        (
            "forged signature",
            Box::new(|_: &str| TokenSpec {
                forge_signature: true,
                ..TokenSpec::valid("ana@corp.com")
            }),
        ),
        (
            "expired token",
            Box::new(|_: &str| TokenSpec {
                expires_in: -60,
                ..TokenSpec::valid("ana@corp.com")
            }),
        ),
        (
            "wrong audience",
            Box::new(|_: &str| TokenSpec {
                audience: Some("some-other-client".to_string()),
                ..TokenSpec::valid("ana@corp.com")
            }),
        ),
        (
            "wrong issuer",
            Box::new(|_: &str| TokenSpec {
                issuer: Some("https://accounts.evil.example".to_string()),
                ..TokenSpec::valid("ana@corp.com")
            }),
        ),
        (
            "unknown signing key",
            Box::new(|_: &str| TokenSpec {
                unknown_kid: true,
                ..TokenSpec::valid("ana@corp.com")
            }),
        ),
        (
            "unverified email",
            Box::new(|_: &str| TokenSpec {
                email_verified: false,
                ..TokenSpec::valid("ana@corp.com")
            }),
        ),
        (
            "mismatched nonce",
            Box::new(|_: &str| TokenSpec {
                nonce: Some("a-nonce-from-another-flow".to_string()),
                ..TokenSpec::valid("ana@corp.com")
            }),
        ),
    ];

    for (label, spec_for) in cases {
        let sessions_before = runtime.store.count_sessions().expect("counting sessions");

        let response = run_sign_in(&server, &idp, "/docs/guide", |state| spec_for(state)).await;

        assert_eq!(
            response.status(),
            401,
            "{label}: must be refused with 401, got {}",
            response.status()
        );
        assert!(
            session_cookie_value(&response).is_none(),
            "{label}: must not set a session cookie"
        );
        assert_eq!(
            runtime.store.count_sessions().expect("counting sessions"),
            sessions_before,
            "{label}: must not create a session"
        );
    }
}

#[tokio::test]
async fn callback_rejects_unknown_replayed_and_missing_state() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let http = client();

    // Unknown state.
    let response = http
        .get(server.url("/auth/callback?code=whatever&state=never-issued"))
        .send()
        .await
        .expect("callback request");
    assert_eq!(response.status(), 400);
    assert!(session_cookie_value(&response).is_none());

    // Missing state entirely.
    let response = http
        .get(server.url("/auth/callback?code=whatever"))
        .send()
        .await
        .expect("callback request");
    assert_eq!(response.status(), 400);

    // A state that was already redeemed cannot be reused (SEC-3).
    let login = http
        .get(server.url("/auth/login"))
        .send()
        .await
        .expect("login request");
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
    let callback_url = location_of(&authorize);

    let first = http
        .get(&callback_url)
        .send()
        .await
        .expect("first callback");
    assert_eq!(first.status(), 303, "the first redemption succeeds");

    let replay = http
        .get(&callback_url)
        .send()
        .await
        .expect("replayed callback");
    assert_eq!(replay.status(), 400, "a replayed state must be refused");
    assert!(session_cookie_value(&replay).is_none());
}

#[tokio::test]
async fn callback_reports_provider_side_refusal() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;

    let response = client()
        .get(server.url("/auth/callback?error=access_denied&state=x"))
        .send()
        .await
        .expect("callback request");

    assert_eq!(response.status(), 401);
    assert!(
        response
            .text()
            .await
            .unwrap_or_default()
            .contains("access_denied"),
        "the visitor should learn that consent was declined"
    );
}

// ---------------------------------------------------------------------------
// US-7: session lifecycle and logout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn logout_deletes_the_session_and_defeats_replay() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let runtime = server.state.auth.as_ref().expect("auth enabled");
    let cookie = sign_in(&server, &idp, "ana@corp.com").await;

    let response = client()
        .post(server.url("/auth/logout"))
        .header(
            reqwest::header::COOKIE,
            format!("{SESSION_COOKIE}={cookie}"),
        )
        .send()
        .await
        .expect("logout request");
    assert_eq!(response.status(), 303);

    let cleared = set_cookie_headers(&response)
        .into_iter()
        .find(|header| header.starts_with(SESSION_COOKIE))
        .expect("a cookie-clearing header");
    assert!(cleared.contains("Max-Age=0"), "got {cleared}");

    // The old value must be dead server-side, not merely dropped by the browser.
    assert!(
        matches!(
            runtime.resolve_session(&cookie).await,
            SessionOutcome::Anonymous
        ),
        "a logged-out session id must not be replayable"
    );
}

#[tokio::test]
async fn unknown_and_tampered_cookies_are_anonymous_not_errors() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let runtime = server.state.auth.as_ref().expect("auth enabled");

    for value in [
        "",
        "not-a-session",
        "../../etc/passwd",
        "'; DROP TABLE sessions;--",
    ] {
        assert!(
            matches!(
                runtime.resolve_session(value).await,
                SessionOutcome::Anonymous
            ),
            "cookie value {value:?} should resolve to anonymous"
        );
    }

    // A tampered value must not disturb a legitimate session.
    let cookie = sign_in(&server, &idp, "ana@corp.com").await;
    assert!(matches!(
        runtime.resolve_session(&cookie).await,
        SessionOutcome::Active(_)
    ));
}

#[tokio::test]
async fn a_session_past_its_maximum_age_is_rejected() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let runtime = server.state.auth.as_ref().expect("auth enabled");
    let cookie = sign_in(&server, &idp, "ana@corp.com").await;

    let max_age_ms = runtime.settings.session_max_age.as_millis() as i64;
    let now = now_ms();
    // Created beyond the ceiling, but seen recently: activity must not extend it (D26).
    runtime
        .store
        .backdate_session(&cookie, now - max_age_ms - 1_000, now)
        .expect("backdating the session");

    assert!(
        matches!(
            runtime.resolve_session(&cookie).await,
            SessionOutcome::Anonymous
        ),
        "an over-age session must be refused however active it has been"
    );
    assert!(
        runtime.store.get_session(&cookie).unwrap().is_none(),
        "the expired session row must be removed"
    );
}

// ---------------------------------------------------------------------------
// US-8: idle-resume revalidation, fail closed (D20/D21)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_recently_active_session_is_not_revalidated() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let runtime = server.state.auth.as_ref().expect("auth enabled");
    let cookie = sign_in(&server, &idp, "ana@corp.com").await;

    let refreshes_before = idp.refresh_calls();
    assert!(matches!(
        runtime.resolve_session(&cookie).await,
        SessionOutcome::Active(_)
    ));
    assert_eq!(
        idp.refresh_calls(),
        refreshes_before,
        "D21: a session inside the idle window must not contact the provider"
    );
}

#[tokio::test]
async fn an_idle_session_is_revalidated_and_survives_a_healthy_provider() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let runtime = server.state.auth.as_ref().expect("auth enabled");
    let cookie = sign_in(&server, &idp, "ana@corp.com").await;

    let now = now_ms();
    runtime
        .store
        .backdate_session(
            &cookie,
            now,
            now - IDLE_REFRESH_AFTER.as_millis() as i64 - 1_000,
        )
        .expect("backdating the session");

    let refreshes_before = idp.refresh_calls();
    assert!(
        matches!(
            runtime.resolve_session(&cookie).await,
            SessionOutcome::Active(_)
        ),
        "a healthy re-validation must keep the session"
    );
    assert_eq!(
        idp.refresh_calls(),
        refreshes_before + 1,
        "D21: an idle session must be re-validated exactly once"
    );
}

#[tokio::test]
async fn an_idle_session_dies_when_the_provider_rejects_the_grant() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let runtime = server.state.auth.as_ref().expect("auth enabled");
    let cookie = sign_in(&server, &idp, "ana@corp.com").await;

    let now = now_ms();
    runtime
        .store
        .backdate_session(
            &cookie,
            now,
            now - IDLE_REFRESH_AFTER.as_millis() as i64 - 1_000,
        )
        .expect("backdating the session");

    // The account was suspended, deleted, or consent was withdrawn.
    idp.set_behaviour(TokenBehaviour::InvalidGrant);

    assert!(
        matches!(
            runtime.resolve_session(&cookie).await,
            SessionOutcome::Anonymous
        ),
        "an explicitly rejected grant must end the session"
    );
    assert!(
        runtime.store.get_session(&cookie).unwrap().is_none(),
        "the rejected session row must be removed"
    );
}

/// D20, and the accepted risk R1: an unreachable provider is *also* fatal to the
/// session. This test exists as much to document that as to verify it — if it ever
/// starts failing because someone made outages non-fatal, that is a spec change.
#[tokio::test]
async fn an_idle_session_dies_when_the_provider_is_unreachable() {
    let idp = MockIdp::start().await;
    let server = TestServer::start_with_auth(VAULT, &idp).await;
    let runtime = server.state.auth.as_ref().expect("auth enabled");
    let cookie = sign_in(&server, &idp, "ana@corp.com").await;

    let now = now_ms();
    runtime
        .store
        .backdate_session(
            &cookie,
            now,
            now - IDLE_REFRESH_AFTER.as_millis() as i64 - 1_000,
        )
        .expect("backdating the session");

    idp.set_behaviour(TokenBehaviour::ServerError);

    assert!(
        matches!(
            runtime.resolve_session(&cookie).await,
            SessionOutcome::Anonymous
        ),
        "D20: a provider outage fails closed"
    );
    assert!(runtime.store.get_session(&cookie).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// NFR-2: an unauthenticated server is unchanged
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unauthenticated_server_exposes_no_auth_routes() {
    let server = TestServer::start_public(VAULT).await;
    let http = client();

    for path in ["/auth/login", "/auth/callback", "/auth/logout"] {
        let response = http
            .get(server.url(path))
            .send()
            .await
            .expect("request to an auth path");
        assert_eq!(
            response.status(),
            404,
            "{path} must not exist without --auth google"
        );
    }

    // And ordinary content is served exactly as before.
    let page = http
        .get(server.url("/docs/guide"))
        .send()
        .await
        .expect("content request");
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("Guide"));
}
