//! The `/auth/*` endpoints.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use tracing::warn;

use crate::server::AppState;

use super::{AuthSettings, SESSION_COOKIE, crypto, sanitize_next};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout))
        // A GET logout keeps the "switch account" link on the deny page working as a
        // plain anchor. It clears local state only; it is not a state-changing action
        // against any other origin.
        .route("/auth/logout", get(logout))
}

/// Build the session cookie. `Secure` is set only on an HTTPS origin, so local
/// development over loopback keeps working (SEC-5).
fn session_cookie(settings: &AuthSettings, value: String) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, value))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(settings.is_secure_origin())
        .max_age(cookie::time::Duration::seconds(
            settings.session_max_age.as_secs() as i64,
        ))
        .build()
}

fn cleared_cookie(settings: &AuthSettings) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, ""))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(settings.is_secure_origin())
        .max_age(cookie::time::Duration::seconds(0))
        .build()
}

async fn login(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(runtime) = state.auth.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let requested_next = params.get("next").map(String::as_str).unwrap_or("/");
    let Some(next) = sanitize_next(requested_next) else {
        // SEC-4: refuse rather than silently rewriting, so a broken link is visible
        // instead of quietly sending the visitor somewhere else after sign-in.
        return (
            StatusCode::BAD_REQUEST,
            "`next` must be a path on this site, such as /docs/page",
        )
            .into_response();
    };

    let verifier = crypto::random_token(48);
    let challenge = crypto::pkce_challenge(&verifier);
    let nonce = crypto::random_token(24);
    let state_token = runtime.begin_flow(verifier, nonce.clone(), next);

    let url = runtime.provider.authorization_url(
        &runtime.credentials.client_id,
        &runtime.settings.redirect_uri(),
        &state_token,
        &nonce,
        &challenge,
    );
    Redirect::to(&url).into_response()
}

async fn callback(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(runtime) = state.auth.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // The provider reports user-side failures (consent denied, for example) here.
    if let Some(error) = params.get("error") {
        return (
            StatusCode::UNAUTHORIZED,
            format!("Sign-in did not complete: {error}"),
        )
            .into_response();
    }

    let (Some(code), Some(state_token)) = (params.get("code"), params.get("state")) else {
        return (StatusCode::BAD_REQUEST, "missing `code` or `state`").into_response();
    };

    // SEC-3. Unknown, replayed, and expired states are indistinguishable to the caller.
    let Some((verifier, nonce, next)) = runtime.take_flow(state_token) else {
        return (
            StatusCode::BAD_REQUEST,
            "This sign-in link has expired or was already used. Please start again.",
        )
            .into_response();
    };

    let tokens = match runtime
        .provider
        .exchange_code(
            &runtime.credentials.client_id,
            &runtime.credentials.client_secret,
            &runtime.settings.redirect_uri(),
            code,
            &verifier,
        )
        .await
    {
        Ok(tokens) => tokens,
        Err(error) => {
            warn!(%error, "authorization code exchange failed");
            return (StatusCode::UNAUTHORIZED, "Sign-in failed.").into_response();
        }
    };

    let Some(id_token) = tokens.id_token.as_deref() else {
        warn!("token response contained no id_token");
        return (StatusCode::UNAUTHORIZED, "Sign-in failed.").into_response();
    };

    let identity = match runtime
        .provider
        .verify_id_token(id_token, &runtime.credentials.client_id, Some(&nonce))
        .await
    {
        Ok(identity) => identity,
        Err(error) => {
            // Covers a bad signature, wrong audience or issuer, expiry, nonce mismatch,
            // and an unverified address (SEC-1, SEC-2).
            warn!(%error, "ID token verification failed");
            return (StatusCode::UNAUTHORIZED, "Sign-in failed.").into_response();
        }
    };

    let session_id = match runtime.create_session(&identity.email, tokens.refresh_token.as_deref())
    {
        Ok(id) => id,
        Err(error) => {
            warn!(%error, "creating the session failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Sign-in failed.").into_response();
        }
    };

    let jar = jar.add(session_cookie(&runtime.settings, session_id));
    (jar, Redirect::to(&next)).into_response()
}

async fn logout(State(state): State<Arc<AppState>>, jar: CookieJar) -> Response {
    let Some(runtime) = state.auth.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if let Some(cookie) = jar.get(SESSION_COOKIE)
        && let Err(error) = runtime.end_session(cookie.value())
    {
        warn!(%error, "deleting the session on logout failed");
    }
    let jar = jar.add(cleared_cookie(&runtime.settings));
    (jar, Redirect::to("/")).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn settings(public_url: &str) -> AuthSettings {
        AuthSettings {
            session_max_age: Duration::from_secs(30 * 86_400),
            audit_retention: Duration::from_secs(90 * 86_400),
            owner_email: None,
            public_url: public_url.to_string(),
            database_path: PathBuf::from("/tmp/mdshelf.db"),
            key_file_path: PathBuf::from("/tmp/secret.key"),
            bad_link_retention: Duration::from_secs(7 * 86_400),
            links: crate::links::LinkSettings::default(),
        }
    }

    /// SEC-5, in the direction the integration tests cannot reach.
    ///
    /// The harness always serves over loopback, so every end-to-end assertion checks
    /// that `Secure` is *absent*. If `is_secure_origin` were inverted, the suite would
    /// stay green while production shipped session cookies without `Secure`.
    #[test]
    fn a_session_cookie_on_an_https_origin_is_marked_secure() {
        let cookie = session_cookie(&settings("https://docs.acme.com"), "abc".into());
        let rendered = cookie.to_string();
        assert!(rendered.contains("Secure"), "got: {rendered}");
        assert!(rendered.contains("HttpOnly"), "got: {rendered}");
        assert!(rendered.contains("SameSite=Lax"), "got: {rendered}");
    }

    #[test]
    fn a_session_cookie_on_a_loopback_origin_is_not_marked_secure() {
        // Secure on plain HTTP would make the cookie unusable and break local dev.
        let cookie = session_cookie(&settings("http://127.0.0.1:4444"), "abc".into());
        let rendered = cookie.to_string();
        assert!(!rendered.contains("Secure"), "got: {rendered}");
        assert!(rendered.contains("HttpOnly"), "got: {rendered}");
    }

    #[test]
    fn the_clearing_cookie_matches_the_attributes_it_must_overwrite() {
        // A browser only replaces a cookie when the attributes line up; a mismatch
        // leaves the original in place and logout silently fails.
        for origin in ["https://docs.acme.com", "http://127.0.0.1:4444"] {
            let live = session_cookie(&settings(origin), "abc".into()).to_string();
            let cleared = cleared_cookie(&settings(origin)).to_string();
            assert_eq!(
                live.contains("Secure"),
                cleared.contains("Secure"),
                "Secure differs between the live and clearing cookie for {origin}"
            );
            assert!(cleared.contains("Path=/"));
            assert!(cleared.contains("Max-Age=0"), "got: {cleared}");
        }
    }
}
