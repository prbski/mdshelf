//! An in-process OpenID Connect issuer for tests.
//!
//! Serves `/.well-known/openid-configuration`, `/jwks`, `/authorize`, and `/token` on an
//! ephemeral port. Tokens are signed with a fixture RSA key whose public half is
//! published in the JWKS, so mdshelf performs genuine RS256 verification.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use serde_json::{Value, json};

/// Fixture keypair. Test-only material, deliberately checked in so the suite is
/// deterministic and needs no key generation at runtime.
const PRIVATE_KEY_PEM: &str = include_str!("../../tests/fixtures/mock_idp_key.pem");
const JWKS_JSON: &str = include_str!("../../tests/fixtures/mock_idp_jwks.json");
const KEY_ID: &str = "mdshelf-mock-key-1";

/// How the issuer should behave when its token endpoint is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenBehaviour {
    /// Issue tokens normally.
    Normal,
    /// Answer `400 invalid_grant` — the account is gone, suspended, or consent was
    /// withdrawn. mdshelf must treat this as a rejection.
    InvalidGrant,
    /// Answer `503` — a provider-side incident. mdshelf must treat this as unreachable.
    ServerError,
}

/// Which properties of a minted ID token to corrupt.
#[derive(Debug, Clone)]
pub struct TokenSpec {
    pub email: String,
    pub email_verified: bool,
    pub audience: Option<String>,
    pub issuer: Option<String>,
    pub nonce: Option<String>,
    /// Seconds from now until expiry. Negative mints an already-expired token.
    pub expires_in: i64,
    /// Sign with a different key so the signature cannot verify against the JWKS.
    pub forge_signature: bool,
    /// Publish a `kid` that is not in the JWKS.
    pub unknown_kid: bool,
}

impl TokenSpec {
    pub fn valid(email: &str) -> Self {
        Self {
            email: email.to_string(),
            email_verified: true,
            audience: None,
            issuer: None,
            nonce: None,
            expires_in: 3600,
            forge_signature: false,
            unknown_kid: false,
        }
    }
}

struct IdpState {
    issuer: String,
    /// Audience minted into tokens; set to the client id mdshelf is configured with.
    default_audience: Mutex<String>,
    behaviour: Mutex<TokenBehaviour>,
    /// `code` -> the token spec to issue when that code is redeemed.
    codes: Mutex<HashMap<String, TokenSpec>>,
    /// Nonce captured from the most recent `/authorize` call, keyed by state.
    nonces: Mutex<HashMap<String, String>>,
    token_calls: Mutex<u32>,
    refresh_calls: Mutex<u32>,
}

/// A running mock issuer. Dropping it leaves the task to be reaped with the runtime.
pub struct MockIdp {
    pub base_url: String,
    state: Arc<IdpState>,
}

impl MockIdp {
    /// Bind an ephemeral port and start serving.
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binding the mock issuer");
        let addr: SocketAddr = listener.local_addr().expect("mock issuer address");
        let base_url = format!("http://{addr}");

        let state = Arc::new(IdpState {
            issuer: base_url.clone(),
            default_audience: Mutex::new("test-client-id".to_string()),
            behaviour: Mutex::new(TokenBehaviour::Normal),
            codes: Mutex::new(HashMap::new()),
            nonces: Mutex::new(HashMap::new()),
            token_calls: Mutex::new(0),
            refresh_calls: Mutex::new(0),
        });

        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/authorize", get(authorize))
            .route("/token", post(token))
            .with_state(Arc::clone(&state));

        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Self { base_url, state }
    }

    pub fn discovery_url(&self) -> String {
        format!("{}/.well-known/openid-configuration", self.base_url)
    }

    pub fn set_audience(&self, audience: &str) {
        *lock(&self.state.default_audience) = audience.to_string();
    }

    pub fn set_behaviour(&self, behaviour: TokenBehaviour) {
        *lock(&self.state.behaviour) = behaviour;
    }

    /// Queue the token that will be issued when `code` is redeemed.
    pub fn register_code(&self, code: &str, spec: TokenSpec) {
        lock(&self.state.codes).insert(code.to_string(), spec);
    }

    /// The nonce mdshelf sent for a given `state`, captured at `/authorize`.
    pub fn nonce_for_state(&self, state: &str) -> Option<String> {
        lock(&self.state.nonces).get(state).cloned()
    }

    pub fn token_calls(&self) -> u32 {
        *lock(&self.state.token_calls)
    }

    pub fn refresh_calls(&self) -> u32 {
        *lock(&self.state.refresh_calls)
    }

    /// Mint a token directly, for tests that verify the verifier rather than the flow.
    pub fn mint(&self, spec: &TokenSpec) -> String {
        mint_token(&self.state, spec)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn discovery(State(state): State<Arc<IdpState>>) -> Json<Value> {
    Json(json!({
        "issuer": state.issuer,
        "authorization_endpoint": format!("{}/authorize", state.issuer),
        "token_endpoint": format!("{}/token", state.issuer),
        "jwks_uri": format!("{}/jwks", state.issuer),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
}

async fn jwks() -> Response {
    let value: Value = serde_json::from_str(JWKS_JSON).expect("fixture JWKS parses");
    Json(value).into_response()
}

/// Record the nonce mdshelf generated, then hand back a code via the redirect, exactly
/// as a real provider would after the user consents.
async fn authorize(
    State(state): State<Arc<IdpState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(redirect_uri) = params.get("redirect_uri") else {
        return (StatusCode::BAD_REQUEST, "missing redirect_uri").into_response();
    };
    let state_token = params.get("state").cloned().unwrap_or_default();
    if let Some(nonce) = params.get("nonce") {
        lock(&state.nonces).insert(state_token.clone(), nonce.clone());
    }

    // Codes are deterministic per state so a test can predict and register them.
    let code = format!("code-for-{state_token}");
    let separator = if redirect_uri.contains('?') { '&' } else { '?' };
    let location = format!("{redirect_uri}{separator}code={code}&state={state_token}");
    axum::response::Redirect::to(&location).into_response()
}

#[derive(Serialize)]
struct TokenResponseBody {
    access_token: String,
    token_type: &'static str,
    expires_in: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    id_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

async fn token(State(state): State<Arc<IdpState>>, body: String) -> Response {
    let params: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();

    let is_refresh = params.get("grant_type").map(String::as_str) == Some("refresh_token");
    if is_refresh {
        *lock(&state.refresh_calls) += 1;
    } else {
        *lock(&state.token_calls) += 1;
    }

    match *lock(&state.behaviour) {
        TokenBehaviour::InvalidGrant => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "invalid_grant",
                    "error_description": "Token has been expired or revoked."
                })),
            )
                .into_response();
        }
        TokenBehaviour::ServerError => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "backend_error" })),
            )
                .into_response();
        }
        TokenBehaviour::Normal => {}
    }

    if is_refresh {
        return Json(TokenResponseBody {
            access_token: "mock-access-token".into(),
            token_type: "Bearer",
            expires_in: 3600,
            id_token: None,
            refresh_token: None,
        })
        .into_response();
    }

    let Some(code) = params.get("code") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_request" })),
        )
            .into_response();
    };

    let spec = lock(&state.codes).get(code).cloned();
    let Some(mut spec) = spec else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant" })),
        )
            .into_response();
    };

    // Adopt the nonce mdshelf sent for this flow unless the test pinned one, so the
    // happy path succeeds without the test having to thread the nonce through.
    if spec.nonce.is_none() {
        let state_token = code.strip_prefix("code-for-").unwrap_or_default();
        spec.nonce = lock(&state.nonces).get(state_token).cloned();
    }

    let id_token = mint_token(&state, &spec);
    Json(TokenResponseBody {
        access_token: "mock-access-token".into(),
        token_type: "Bearer",
        expires_in: 3600,
        id_token: Some(id_token),
        refresh_token: Some("mock-refresh-token".into()),
    })
    .into_response()
}

fn mint_token(state: &IdpState, spec: &TokenSpec) -> String {
    let now = jsonwebtoken::get_current_timestamp() as i64;
    let audience = spec
        .audience
        .clone()
        .unwrap_or_else(|| lock(&state.default_audience).clone());
    let issuer = spec.issuer.clone().unwrap_or_else(|| state.issuer.clone());

    let mut claims = json!({
        "iss": issuer,
        "aud": audience,
        "sub": format!("sub-{}", spec.email),
        "exp": now + spec.expires_in,
        "iat": now,
        "email": spec.email,
        "email_verified": spec.email_verified,
    });
    if let Some(nonce) = spec.nonce.as_ref() {
        claims["nonce"] = json!(nonce);
    }

    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(if spec.unknown_kid {
        "not-a-published-key".to_string()
    } else {
        KEY_ID.to_string()
    });

    let key = if spec.forge_signature {
        // A structurally valid RS256 signature made with the wrong key: the shape is
        // right, so only real signature verification rejects it.
        EncodingKey::from_rsa_pem(FORGED_KEY_PEM.as_bytes()).expect("forged fixture key parses")
    } else {
        EncodingKey::from_rsa_pem(PRIVATE_KEY_PEM.as_bytes()).expect("fixture key parses")
    };

    encode(&header, &claims, &key).expect("minting the mock ID token")
}

const FORGED_KEY_PEM: &str = include_str!("../../tests/fixtures/mock_idp_forged_key.pem");
