//! The session-authenticated share endpoints (US-15 … US-17).
//!
//! Everything here belongs to a signed-in viewer acting on their own behalf. The issuer
//! recorded on a link is always the address on the session cookie — never a value taken
//! from the request body — because a body-supplied issuer would let anyone mint a link
//! in somebody else's name, and S29 would then hand out that person's access.
//!
//! SEC-7: the cross-site defence is `SameSite=Lax` on the session cookie, which keeps
//! the browser from attaching it to a cross-site POST at all, plus axum's `Json`
//! extractor requiring `Content-Type: application/json`, which an HTML form cannot send.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use crate::auth::store::now_ms;
use crate::links::pages::ShareRow;
use crate::server::AppState;
use crate::server::routes::Viewer;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/__share", post(mint))
        .route("/__share/revoke", post(revoke))
        .route("/__shares", get(shares))
}

#[derive(Deserialize)]
struct MintRequest {
    /// The page to share, as its own URL. Never an issuer: that comes from the session.
    url: String,
    #[serde(default, rename = "for")]
    for_duration: Option<String>,
    #[serde(default)]
    until: Option<String>,
}

#[derive(Deserialize)]
struct RevokeRequest {
    id: String,
}

/// The one answer to "that page does not exist" and "you may not read that page".
///
/// US-15 requires them to be identical; keeping them in one function is what makes that
/// true rather than merely intended.
fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "not found" })),
    )
        .into_response()
}

fn refused() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "sign in first" })),
    )
        .into_response()
}

fn bad_request(message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

/// `POST /__share` (US-15).
async fn mint(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Json(request): Json<MintRequest>,
) -> Response {
    let Some(runtime) = state.auth.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Viewer::Signed(issuer) = crate::server::routes::resolve_viewer(&state, &jar).await else {
        return refused();
    };
    if !runtime.settings.links.enabled {
        return not_found();
    }

    // Resolve the page inside the *viewer's own* projection, so a page they cannot read
    // is indistinguishable from one that does not exist (S8/US-15).
    let target = {
        let universe = state.universe.read().await;
        crate::links::commands::resolve_target(&universe, &request.url)
            .ok()
            .filter(|target| target.site.allows_path(&target.rel_path, Some(&issuer)))
            .map(|target| {
                (
                    crate::links::commands::site_key(&target.site),
                    crate::content::rel_path_key(&target.rel_path),
                )
            })
    };
    let Some((site, path)) = target else {
        return not_found();
    };

    let now = now_ms();
    let expires_at = match crate::links::resolve_expiry(
        &runtime.settings.links,
        request.for_duration.as_deref(),
        request.until.as_deref(),
        now,
    ) {
        Ok(expires_at) => expires_at,
        // The sharer's own input, past the point where the page check already answered
        // identically for every page they may not see, so a real message is safe here.
        Err(error) => return bad_request(error.to_string()),
    };

    let token = match crate::links::mint(&runtime.store, &site, &path, expires_at, now, &issuer) {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(%error, "minting a share link failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "could not create a link" })),
            )
                .into_response();
        }
    };

    // The one moment the plaintext token exists outside the recipient's browser (S5).
    let url = runtime
        .settings
        .links
        .url(&runtime.settings.public_url, &token);
    Json(serde_json::json!({
        "url": url,
        "id": token.id(),
        "expires_at": expires_at,
        "expires_in": crate::links::time::humanize_remaining(expires_at - now),
    }))
    .into_response()
}

/// `POST /__share/revoke` (US-16).
///
/// Issuer-scoped: revoking somebody else's link answers exactly like revoking one that
/// does not exist, so the endpoint cannot be used to probe for other people's links.
/// The CLI is the escape hatch for revoking anything (S11).
async fn revoke(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Json(request): Json<RevokeRequest>,
) -> Response {
    let Some(runtime) = state.auth.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Viewer::Signed(issuer) = crate::server::routes::resolve_viewer(&state, &jar).await else {
        return refused();
    };

    match runtime
        .store
        .revoke_link_for_issuer(&request.id, &issuer, now_ms())
    {
        Ok(true) => Json(serde_json::json!({ "revoked": request.id })).into_response(),
        Ok(false) => not_found(),
        Err(error) => {
            tracing::warn!(%error, "revoking a share link failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "could not revoke that link" })),
            )
                .into_response()
        }
    }
}

/// `GET /__shares` (US-17).
async fn shares(State(state): State<Arc<AppState>>, jar: CookieJar) -> Response {
    if state.auth.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let viewer = crate::server::routes::resolve_viewer(&state, &jar).await;
    let Viewer::Signed(email) = viewer else {
        // An anonymous visitor is offered sign-in, exactly as for any other path.
        return crate::server::routes::interstitial_response(&state);
    };
    let runtime = state.auth.as_ref().expect("auth on");

    let now = now_ms();
    let links = runtime
        .store
        .list_links(now, false, Some(&email))
        .unwrap_or_default();

    let rows = {
        let universe = state.universe.read().await;
        links
            .iter()
            .map(|record| ShareRow {
                id: record.id.clone(),
                page: crate::links::commands::page_url(&universe, record)
                    // A link whose site was unconfigured out from under it still has to
                    // be listed, or it becomes exposure nobody can see or revoke.
                    .unwrap_or_else(|| format!("{}:{}", record.site, record.path)),
                expires_in: crate::links::time::humanize_remaining(record.expires_at - now),
            })
            .collect::<Vec<_>>()
    };

    Html(crate::links::pages::shares_page(&email, &rows)).into_response()
}
