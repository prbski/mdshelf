//! Serving a share link (US-7 … US-12).
//!
//! Every response this module can produce is one of exactly two things: the one page a
//! link names (or a file that page references), or the deny page. There is no third
//! outcome and no error page, because SEC-3 requires expired, revoked, unknown,
//! malformed and nonexistent to be byte-identical — and the only way to be sure of that
//! is for every failure to reach the same `return`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path as UrlPath, State, ws::WebSocketUpgrade},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};

use crate::auth::store::{LinkRecord, Outcome, now_ms};
use crate::content::page::rewrite_attribute_urls;
use crate::content::{Site, Universe};
use crate::links::{BAD_LINK_PSEUDONYM, is_wellformed_token, link_pseudonym, token_hash};
use crate::render::templates::{
    LinkBannerContext, LinkPageContext, LinkSiteContext, LinkTemplateContext,
};
use crate::server::AppState;

/// The routes under `[links] prefix`. Only mounted when auth is on (S16/NFR-1).
pub fn router(prefix: &str) -> Router<Arc<AppState>> {
    Router::new()
        .route(prefix, get(bare_prefix))
        .route(&format!("{prefix}/{{token}}"), get(serve_page))
        // Registered before the catch-all so a page's own reload socket wins over an
        // asset that happens to be called `__reload`.
        .route(&format!("{prefix}/{{token}}/__reload"), get(reload_socket))
        .route(&format!("{prefix}/{{token}}/{{*asset}}"), get(serve_asset))
}

/// A request path with any share token replaced by a placeholder (SEC-2).
///
/// The HTTP trace layer logs every request URI at DEBUG, and a link URL *is* its token,
/// so without this the one thing SEC-2 forbids is written to the log on every single
/// link read. The rest of the path is kept: it is the asset name, which is useful and
/// is not a credential.
pub fn redact_uri(path: &str, prefix: &str) -> String {
    let Some(tail) = path.strip_prefix(prefix) else {
        return path.to_string();
    };
    let Some(rest) = tail.strip_prefix('/') else {
        // `/s` itself, or an unrelated path like `/system` that merely shares a prefix.
        return path.to_string();
    };
    match rest.split_once('/') {
        Some((_token, remainder)) => format!("{prefix}/<token>/{remainder}"),
        None => format!("{prefix}/<token>"),
    }
}

/// The headers every link response carries (S17/SEC-8).
///
/// `no-store` is the load-bearing one: without it a shared cache could hold a
/// token-bearing response and hand it to somebody else.
fn link_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static("noindex, nofollow"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers
}

/// The single answer to every failure (S13/SEC-3).
pub fn deny_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        link_headers(),
        Html(crate::links::pages::denied()),
    )
        .into_response()
}

async fn bare_prefix() -> Response {
    deny_response()
}

/// What a token resolved to.
struct ResolvedLink {
    record: LinkRecord,
    /// URL the page is served at for signed-in readers; also the access-log path, so
    /// link reads and signed-in reads join by eye (US-11).
    page_url: String,
    title: String,
    html: String,
    assets: BTreeSet<String>,
    color: String,
    mount: String,
    root: PathBuf,
}

/// Steps 1–5 of §6.3, in one place.
///
/// Returns `None` for every reason a link does not serve. The caller must not learn
/// which reason it was — that distinction is exactly what SEC-3 forbids leaking — so
/// the access-log row is written here, where the reason is still known.
async fn resolve(state: &Arc<AppState>, token: &str) -> Option<ResolvedLink> {
    let runtime = state.auth.as_ref()?;
    let settings = &runtime.settings.links;

    // S19: the kill switch. Rows survive, nothing serves, and no row is written for a
    // request the feature is not answering.
    if !settings.enabled {
        return None;
    }

    // A malformed token is answered exactly like an unknown one, and recorded the same
    // way: `bad-link` says a stranger probed the prefix, which is all it needs to say.
    if !is_wellformed_token(token) {
        log_bad_link(state);
        return None;
    }

    let record = match runtime.store.link_by_token_hash(&token_hash(token)) {
        Ok(Some(record)) => record,
        Ok(None) => {
            log_bad_link(state);
            return None;
        }
        Err(error) => {
            // Fail closed (D9). The token may well be genuine, but a database that
            // cannot answer is not grounds for serving anything.
            tracing::warn!(%error, "link lookup failed; denying");
            return None;
        }
    };

    let now = now_ms();
    if !record.is_live(now) {
        log_link(
            state,
            &record,
            page_path_for_log(state, &record).await,
            Outcome::Deny,
        );
        return None;
    }

    let universe = state.universe.read().await;
    let Some(site) = site_for(&universe, &record.site) else {
        // The site was unconfigured out from under the link (S22).
        log_link(state, &record, record.path.clone(), Outcome::Deny);
        return None;
    };
    // S29, the keystone: the link serves only while its issuer can still read the page.
    let Some(page) = site.link_page(&record.path, &record.issued_by) else {
        log_link(state, &record, record.path.clone(), Outcome::Deny);
        return None;
    };

    Some(ResolvedLink {
        page_url: page.url.clone(),
        title: page.title.clone(),
        html: page.html.clone(),
        assets: page.assets.clone(),
        color: site.color.clone(),
        mount: site.mount.clone(),
        root: site.root.clone(),
        record,
    })
}

/// The page path a denied request should be recorded against.
///
/// Falls back to the stored relative path when the page cannot be resolved, so an audit
/// row always names something an operator can act on.
async fn page_path_for_log(state: &Arc<AppState>, record: &LinkRecord) -> String {
    let universe = state.universe.read().await;
    site_for(&universe, &record.site)
        .and_then(|site| {
            site.pages()
                .find(|page| crate::content::rel_path_key(&page.rel_path) == record.path)
                .map(|page| page.url.clone())
        })
        .unwrap_or_else(|| record.path.clone())
}

fn site_for<'a>(universe: &'a Universe, site_key: &str) -> Option<&'a Arc<Site>> {
    universe
        .sites()
        .iter()
        .find(|site| crate::links::commands::site_key(site) == site_key)
}

/// US-7: the reading view.
async fn serve_page(
    State(state): State<Arc<AppState>>,
    UrlPath(token): UrlPath<String>,
) -> Response {
    let Some(resolved) = resolve(&state, &token).await else {
        return deny_response();
    };

    let remaining = resolved.record.expires_at - now_ms();
    let reload_path = format!(
        "{}/{}/__reload",
        state
            .auth
            .as_ref()
            .expect("resolve only succeeds with auth on")
            .settings
            .links
            .prefix,
        token
    );
    let ctx = LinkTemplateContext {
        page: LinkPageContext {
            title: resolved.title.clone(),
            html: tokenize_html(&resolved, &state, &token),
        },
        banner: LinkBannerContext {
            // S26/R1: the sharer's own address reaches everyone the URL reaches.
            issuer: resolved.record.issued_by.clone(),
            expires_in: crate::links::time::humanize_remaining(remaining),
        },
        site: LinkSiteContext {
            color: resolved.color.clone(),
        },
        reload_script: crate::links::pages::reload_script(&reload_path),
        live_reload: state.live_reload_enabled,
        config: crate::server::routes::config_summary(&state),
    };

    let rendered = {
        let renderer = state.renderer.read().await;
        renderer.render_link(&ctx)
    };
    let Ok(html) = rendered else {
        tracing::warn!("rendering the reading view failed; denying");
        return deny_response();
    };

    log_link(
        &state,
        &resolved.record,
        resolved.page_url.clone(),
        Outcome::Allow,
    );
    (StatusCode::OK, link_headers(), Html(html)).into_response()
}

/// US-8: exactly the assets the page references, and nothing else.
async fn serve_asset(
    State(state): State<Arc<AppState>>,
    UrlPath((token, asset)): UrlPath<(String, String)>,
) -> Response {
    let Some(resolved) = resolve(&state, &token).await else {
        return deny_response();
    };

    let Some(relative) = asset_key(&resolved, &asset) else {
        // Another page, the raw markdown source, or a file beside the page that it
        // never mentions. All of them are answered with the deny page (SEC-4).
        log_link(
            &state,
            &resolved.record,
            resolved.page_url.clone(),
            Outcome::Deny,
        );
        return deny_response();
    };

    let full = resolved.root.join(&relative);
    let Ok(bytes) = tokio::fs::read(&full).await else {
        log_link(
            &state,
            &resolved.record,
            resolved.page_url.clone(),
            Outcome::Deny,
        );
        return deny_response();
    };

    let mut headers = link_headers();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(crate::server::routes::content_type_for_path(&full)),
    );
    log_link(
        &state,
        &resolved.record,
        resolved.page_url.clone(),
        Outcome::Allow,
    );
    (StatusCode::OK, headers, bytes).into_response()
}

/// The site-relative path an asset request names, if the page actually references it.
///
/// Resolved through the filesystem first, so a differently-cased spelling on a
/// case-insensitive volume is checked against the file it would really open rather than
/// against the string that was asked for.
fn asset_key(resolved: &ResolvedLink, requested: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(requested)
        .decode_utf8_lossy()
        .into_owned();
    let trimmed = decoded.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return None;
        }
    }
    let true_rel =
        crate::content::source::true_relative_path(&resolved.root, std::path::Path::new(trimmed))?;
    let key = crate::content::rel_path_key(&true_rel);
    // Membership of the referenced set is the whole of the authorization decision here.
    if !resolved.assets.contains(&key) {
        return None;
    }
    Some(key)
}

/// Point every in-vault URL in the page body at this link (US-8).
///
/// Every root-relative URL is rewritten, not just the ones under this site's mount: a
/// URL that kept its original form would be an in-vault URL without a token, which is
/// precisely what the token-completeness test forbids. Rewritten URLs that name
/// something outside the referenced set simply reach the deny page, which is the
/// correct answer for them.
///
/// mdshelf's own routes (`/__assets`, `/__mdshelf`) are left alone: they carry theme
/// files, not vault content, and are already served to everyone.
fn tokenize_html(resolved: &ResolvedLink, state: &Arc<AppState>, token: &str) -> String {
    let prefix = &state
        .auth
        .as_ref()
        .expect("resolve only succeeds with auth on")
        .settings
        .links
        .prefix;
    let link_base = format!("{prefix}/{token}");
    let mount_prefix = if resolved.mount == "/" {
        "/".to_string()
    } else {
        format!("{}/", resolved.mount)
    };
    rewrite_attribute_urls(&resolved.html, |url| {
        if !url.starts_with('/') || url.starts_with("//") || url.starts_with("/__") {
            return None;
        }
        let tail = url
            .strip_prefix(mount_prefix.as_str())
            .unwrap_or_else(|| url.trim_start_matches('/'));
        Some(format!("{link_base}/{tail}"))
    })
}

/// US-12: live reload, for one page, over a socket the same token authorises.
async fn reload_socket(
    State(state): State<Arc<AppState>>,
    UrlPath(token): UrlPath<String>,
    ws: WebSocketUpgrade,
) -> Response {
    if !state.live_reload_enabled {
        return deny_response();
    }
    let Some(resolved) = resolve(&state, &token).await else {
        // An unknown token never gets as far as an upgrade.
        return deny_response();
    };

    let record_id = resolved.record.id.clone();
    let site_key = resolved.record.site.clone();
    let rel_path = resolved.record.path.clone();
    let digest = content_digest(&resolved.html, &resolved.assets);
    let receiver = state.live_reload_tx.subscribe();
    let state = Arc::clone(&state);

    ws.on_upgrade(move |socket| async move {
        run_reload_socket(
            socket, state, receiver, record_id, site_key, rel_path, digest,
        )
        .await;
    })
}

/// A stable fingerprint of what a link currently serves.
///
/// Comparing it is how "editing any other page pushes nothing to it" is enforced: the
/// watcher's reload event says only that *something* changed, so the socket has to work
/// out for itself whether that something was its own page.
fn content_digest(html: &str, assets: &BTreeSet<String>) -> String {
    let mut parts = vec![html.to_string()];
    parts.extend(assets.iter().cloned());
    crate::auth::crypto::signature_digest(&parts)
}

#[allow(clippy::too_many_arguments)]
async fn run_reload_socket(
    socket: axum::extract::ws::WebSocket,
    state: Arc<AppState>,
    mut reload_rx: tokio::sync::broadcast::Receiver<()>,
    link_id: String,
    site_key: String,
    rel_path: String,
    mut digest: String,
) {
    use axum::extract::ws::{CloseFrame, Message};
    use futures::{SinkExt, StreamExt};

    let (mut sender, mut receiver) = socket.split();
    let mut ping_ticker = tokio::time::interval(std::time::Duration::from_secs(20));
    ping_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            event = reload_rx.recv() => {
                match event {
                    Ok(()) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }

                // US-12: revalidate *before* the push, never after. A revoked link is
                // closed rather than fed one last event.
                let Some(current) = current_content(&state, &link_id, &site_key, &rel_path).await
                else {
                    let _ = sender
                        .send(Message::Close(Some(CloseFrame {
                            code: 1008,
                            reason: "link is no longer valid".into(),
                        })))
                        .await;
                    break;
                };
                if current == digest {
                    // Something changed, but not this page.
                    continue;
                }
                digest = current;
                if sender.send(Message::Text("reload".into())).await.is_err() {
                    break;
                }
            }
            _ = ping_ticker.tick() => {
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            client_message = receiver.next() => {
                match client_message {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

/// The current digest of what a link serves, or `None` if it no longer serves anything.
async fn current_content(
    state: &Arc<AppState>,
    link_id: &str,
    site_key: &str,
    rel_path: &str,
) -> Option<String> {
    let runtime = state.auth.as_ref()?;
    if !runtime.settings.links.enabled {
        return None;
    }
    let record = runtime.store.link_by_id(link_id).ok().flatten()?;
    if !record.is_live(now_ms()) {
        return None;
    }
    let universe = state.universe.read().await;
    let site = site_for(&universe, site_key)?;
    let page = site.link_page(rel_path, &record.issued_by)?;
    Some(content_digest(&page.html, &page.assets))
}

/// Record a link read under its pseudonym (S14/US-11).
///
/// The path recorded is the page's own URL, never the request path — a request path
/// under the prefix carries the token, and SEC-2 forbids a token reaching any database
/// column. A failure here is logged and dropped: losing an audit row must never cost a
/// reader their page.
fn log_link(state: &Arc<AppState>, record: &LinkRecord, path: String, outcome: Outcome) {
    let Some(runtime) = state.auth.as_ref() else {
        return;
    };
    if let Err(error) =
        runtime
            .store
            .log_access(&link_pseudonym(&record.id), &path, now_ms(), outcome)
    {
        tracing::warn!(%error, "failed to record a link read");
    }
}

/// Record a probe of the prefix by an unknown token (S15).
///
/// Neither the token nor any prefix of it is written: the row says a stranger tried the
/// prefix, and that is deliberately all it says.
fn log_bad_link(state: &Arc<AppState>) {
    let Some(runtime) = state.auth.as_ref() else {
        return;
    };
    let prefix = runtime.settings.links.prefix.clone();
    if let Err(error) =
        runtime
            .store
            .log_access(BAD_LINK_PSEUDONYM, &prefix, now_ms(), Outcome::BadLink)
    {
        tracing::warn!(%error, "failed to record an unknown-token request");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_never_survives_into_a_logged_uri() {
        assert_eq!(redact_uri("/s/abcdefghij", "/s"), "/s/<token>");
        assert_eq!(
            redact_uri("/s/abcdefghij/img/a.png", "/s"),
            "/s/<token>/img/a.png"
        );
        assert_eq!(
            redact_uri("/s/abcdefghij/__reload", "/s"),
            "/s/<token>/__reload"
        );
        assert_eq!(redact_uri("/s", "/s"), "/s");
    }

    /// Paths outside the prefix are untouched, so ordinary request logging is unchanged
    /// (NFR-1).
    #[test]
    fn other_paths_are_logged_exactly_as_before() {
        for path in [
            "/docs/hr/comp",
            "/system/status",
            "/",
            "/__assets/css/main.css",
        ] {
            assert_eq!(redact_uri(path, "/s"), path);
        }
    }
}
