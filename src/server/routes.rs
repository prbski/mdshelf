use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tower::ServiceBuilder;
use tower_http::{compression::CompressionLayer, trace::TraceLayer};

use axum_extra::extract::cookie::CookieJar;

use crate::auth::SessionOutcome;
use crate::auth::store::Outcome;
use crate::content::page::{humanize, join_url};
use crate::content::tree::{breadcrumbs, breadcrumbs_for_index_path, prev_next};
use crate::content::{
    Page, Site, SiteView, Universe, build_site_index_context, build_site_index_under_prefix,
};
use crate::render::templates::{
    ConfigSummary, Crumb, ErrorTemplateContext, HomeTemplateContext, NeighborContext, PageContext,
    PageTemplateContext, SiteContext, SiteListEntry,
};
use crate::server::AppState;
use crate::server::error::AppError;
use crate::server::livereload;

pub fn router(state: Arc<AppState>) -> Router {
    let mut router = Router::new()
        .route("/", get(home))
        .route("/__mdshelf/syntax.css", get(syntax_css))
        .route("/__assets/{*asset_path}", get(theme_asset))
        .route("/__livereload", get(livereload::livereload_ws));

    // The `/auth/*` endpoints exist only when auth is enabled, so an unauthenticated
    // server exposes exactly the surface it did before (NFR-2).
    if state.auth_enabled() {
        router = router.merge(crate::auth::routes::router());
    }

    router
        .route("/{*rest}", get(site_or_not_found))
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_http())
                .layer(CompressionLayer::new()),
        )
        .with_state(state)
}

async fn home(State(state): State<Arc<AppState>>, jar: CookieJar) -> Result<Response, AppError> {
    // The home page enumerates every configured site and how many pages each holds.
    // It needs the same gate as any other page, or it becomes the one URL that
    // describes the whole server to anyone who asks.
    let viewer = resolve_viewer(&state, &jar).await;
    if matches!(viewer, Viewer::Anonymous) {
        return Ok(interstitial_response(&state));
    }

    let all_sites = {
        let universe = state.universe.read().await;
        site_list_entries_for(&universe, viewer.email())
    };

    // A signed-in viewer with access to nothing must not be able to tell an empty
    // server from one whose contents are simply not theirs (D23).
    if let Viewer::Signed(email) = &viewer
        && all_sites.is_empty()
    {
        return Ok(denied_response(&state, email));
    }
    let ctx = HomeTemplateContext {
        all_sites,
        config: config_summary(&state),
        live_reload: state.live_reload_enabled,
    };
    let mut html = {
        let renderer = state.renderer.read().await;
        renderer.render_home(&ctx).map_err(AppError::from)?
    };
    html = inject_live_reload(html, state.live_reload_enabled);
    Ok(Html(html).into_response())
}

async fn syntax_css(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let css = state.markdown.syntax_css();
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        css.to_string(),
    )
}

async fn theme_asset(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(asset_path): axum::extract::Path<String>,
) -> Result<Response, AppError> {
    let relative = format!("assets/{}", asset_path.trim_start_matches('/'));
    let resolved = state
        .renderer
        .read()
        .await
        .theme()
        .resolve_asset(None, &relative)
        .ok_or_else(|| AppError::not_found(format!("asset not found: {}", relative)))?;
    let mime = content_type_for_path(Path::new(&asset_path));
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    if asset_path == "js/livereload.js" {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    Ok((headers, resolved.bytes).into_response())
}

/// Who is making a request.
#[derive(Debug, Clone)]
pub enum Viewer {
    /// Authorization is not in force on this server (NFR-2).
    Unrestricted,
    /// No usable session; the visitor sees the interstitial for every path (SEC-9).
    Anonymous,
    /// A live session belonging to this verified address.
    Signed(String),
}

impl Viewer {
    /// The address to resolve ACLs against, or `None` when nothing is enforced.
    fn email(&self) -> Option<&str> {
        match self {
            Viewer::Unrestricted => None,
            Viewer::Anonymous => None,
            Viewer::Signed(email) => Some(email.as_str()),
        }
    }
}

async fn resolve_viewer(state: &AppState, jar: &CookieJar) -> Viewer {
    let Some(runtime) = state.auth.as_ref() else {
        return Viewer::Unrestricted;
    };
    let Some(cookie) = jar.get(crate::auth::SESSION_COOKIE) else {
        return Viewer::Anonymous;
    };
    match runtime.resolve_session(cookie.value()).await {
        SessionOutcome::Active(email) => Viewer::Signed(email),
        SessionOutcome::Anonymous => Viewer::Anonymous,
    }
}

/// The sign-in interstitial, served for any path an anonymous visitor asks for.
fn interstitial_response(state: &AppState) -> Response {
    let site_name = state
        .config
        .sites
        .first()
        .and_then(|site| site.title.clone())
        .unwrap_or_else(|| "Private site".to_string());
    (
        StatusCode::UNAUTHORIZED,
        Html(crate::auth::pages::interstitial(&site_name)),
    )
        .into_response()
}

/// The unified deny page (D23).
///
/// Answers both "you may not read this" and "this does not exist" with the same bytes
/// and the same status, so the response cannot be used to enumerate the vault.
fn denied_response(state: &AppState, email: &str) -> Response {
    let owner = state
        .auth
        .as_ref()
        .and_then(|runtime| runtime.settings.owner_email.as_deref());
    (
        StatusCode::NOT_FOUND,
        Html(crate::auth::pages::denied(email, owner)),
    )
        .into_response()
}

/// Record the outcome of a request against the access log (US-21).
fn log_access(state: &AppState, viewer: &Viewer, path: &str, outcome: Outcome) {
    let (Some(runtime), Viewer::Signed(email)) = (state.auth.as_ref(), viewer) else {
        return;
    };
    if let Err(error) = runtime
        .store
        .log_access(email, path, crate::auth::store::now_ms(), outcome)
    {
        // Losing an audit row must never cost the reader their page.
        tracing::warn!(%error, "failed to record an access-log entry");
    }
}

async fn site_or_not_found(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    uri: Uri,
) -> Result<Response, AppError> {
    let raw_path = uri.path();
    if raw_path.starts_with("/__") {
        return Err(AppError::not_found("not found"));
    }

    let viewer = resolve_viewer(&state, &jar).await;
    if matches!(viewer, Viewer::Anonymous) {
        // SEC-9: every path, existing or not, so the interstitial itself reveals nothing.
        return Ok(interstitial_response(&state));
    }

    let path = raw_path.trim_end_matches('/');
    let matched = {
        let universe = state.universe.read().await;
        // The site switcher is built here, where the viewer is known. Building it inside
        // `match_site_path` meant every content page listed every configured site,
        // including ones the viewer has no access to.
        match_site_path(&universe, path)
            .map(|(site, tail)| (site, tail, site_list_entries_for(&universe, viewer.email())))
    };

    let Some((site, tail, all_sites)) = matched else {
        // A path under no configured site. For a signed-in viewer this must be
        // indistinguishable from a path they simply may not read (D23).
        if let Viewer::Signed(email) = &viewer {
            log_access(&state, &viewer, raw_path, Outcome::Deny);
            return Ok(denied_response(&state, email));
        }
        let body = render_error_page(
            &state,
            404,
            "Not found",
            "No configured site matches this URL.",
        )
        .await?;
        return Ok((StatusCode::NOT_FOUND, Html(body)).into_response());
    };
    serve_site_request(&state, site, &tail, &all_sites, &viewer, raw_path).await
}

/// Find the site that owns `request_path`, and the tail within it.
///
/// Deliberately does not build the site switcher: that depends on who is asking, and
/// this function does not know.
fn match_site_path(universe: &Universe, request_path: &str) -> Option<(Arc<Site>, String)> {
    let path = request_path.split('?').next().unwrap_or(request_path);
    let mut candidates: Vec<(Arc<Site>, &str)> = Vec::new();
    for site in universe.sites() {
        let mount = site.mount.as_str();
        if path == mount {
            candidates.push((Arc::clone(site), ""));
        } else if let Some(prefix) = path.strip_prefix(mount) {
            if prefix.starts_with('/') {
                let tail = prefix.trim_start_matches('/');
                candidates.push((Arc::clone(site), tail));
            }
        }
    }
    candidates.sort_by(|a, b| b.0.mount.len().cmp(&a.0.mount.len()));
    let (site, tail) = candidates.into_iter().next()?;
    Some((site, tail.to_string()))
}

async fn serve_site_request(
    state: &Arc<AppState>,
    site: Arc<Site>,
    tail: &str,
    all_sites: &[SiteListEntry],
    viewer: &Viewer,
    request_path: &str,
) -> Result<Response, AppError> {
    let normalized = normalize_tail(tail);

    // Everything below reads from `view`, the viewer's projection of the site. A page,
    // navigation entry, or index row that is not in it cannot be rendered by any code
    // path here (US-16, US-17).
    let view = site.view(viewer.email());

    // Anything that does not produce a real, permitted page for a signed-in viewer ends
    // at the same deny page, so "restricted" and "missing" are indistinguishable (D23).
    let deny = |state: &Arc<AppState>| -> Option<Response> {
        match viewer {
            Viewer::Signed(email) => Some(denied_response(state, email)),
            _ => None,
        }
    };

    if normalized.trim().is_empty() && view.page("").is_none() {
        // With auth on, an auto-generated listing of the whole site is exactly the
        // enumeration surface D23 exists to close — but built from the filtered view it
        // lists only what this viewer may already read.
        if view.pages().next().is_none()
            && let Some(response) = deny(state)
        {
            log_access(state, viewer, request_path, Outcome::Deny);
            return Ok(response);
        }
        let site_index = build_site_index_context(view.pages_map(), site.root.as_path());
        let root_url = join_url(site.mount.as_str(), "");
        let ctx = PageTemplateContext {
            site: SiteContext {
                title: site.title.clone(),
                mount: site.mount.clone(),
                root_url: site.mount.clone(),
                color: site.color.clone(),
            },
            page: PageContext {
                title: site.title.clone(),
                description: Some(
                    "Overview of every page in this site. Add index.md at the site root to replace this page."
                        .to_string(),
                ),
                url: root_url.clone(),
                url_path: String::new(),
                layout: "doc".to_string(),
                draft: false,
                headings: vec![],
                frontmatter: serde_json::json!({}),
                html: String::new(),
            },
            nav_flat: view.nav_flat(),
            breadcrumbs: vec![Crumb {
                title: site.title.clone(),
                url: root_url,
            }],
            prev: None,
            next: None,
            site_index: Some(site_index),
            all_sites: all_sites.to_vec(),
            config: config_summary(state),
            live_reload: state.live_reload_enabled,
        };
        let mut html = {
            let renderer = state.renderer.read().await;
            renderer.render_page(&ctx).map_err(AppError::from)?
        };
        html = inject_live_reload(html, state.live_reload_enabled);
        return Ok(Html(html).into_response());
    }

    if let Some(page) = resolve_markdown_page(&view, &normalized) {
        if page.draft {
            // A draft is "not published", which for a signed-in viewer must look the
            // same as everything else they cannot read (D23).
            if let Some(response) = deny(state) {
                log_access(state, viewer, request_path, Outcome::Deny);
                return Ok(response);
            }
            return Err(AppError::not_found("draft"));
        }
        log_access(state, viewer, request_path, Outcome::Allow);
        let crumbs = breadcrumbs(
            site.title.as_str(),
            site.mount.as_str(),
            view.pages_map(),
            page,
        );
        let (prev_page, next_page) = prev_next(view.pages_map(), page);
        let prev = prev_page.map(|p| NeighborContext {
            title: p.title.clone(),
            url: p.url.clone(),
        });
        let next = next_page.map(|p| NeighborContext {
            title: p.title.clone(),
            url: p.url.clone(),
        });
        let ctx = PageTemplateContext {
            site: SiteContext {
                title: site.title.clone(),
                mount: site.mount.clone(),
                root_url: site.mount.clone(),
                color: site.color.clone(),
            },
            page: PageContext {
                title: page.title.clone(),
                description: page.description.clone(),
                url: page.url.clone(),
                url_path: page.url_path.clone(),
                layout: page.layout.clone(),
                draft: page.draft,
                headings: page.headings.clone(),
                frontmatter: page.frontmatter.clone(),
                html: page.html.clone(),
            },
            nav_flat: view.nav_flat(),
            breadcrumbs: crumbs,
            prev,
            next,
            site_index: None,
            all_sites: all_sites.to_vec(),
            config: config_summary(state),
            live_reload: state.live_reload_enabled,
        };
        let mut html = {
            let renderer = state.renderer.read().await;
            renderer.render_page(&ctx).map_err(AppError::from)?
        };
        html = inject_live_reload(html, state.live_reload_enabled);
        return Ok(Html(html).into_response());
    }

    let folder_key = normalized.trim_matches('/').to_string();
    if !folder_key.is_empty() {
        if let Some(folder_site_index) = build_site_index_under_prefix(
            view.pages_map(),
            folder_key.as_str(),
            site.root.as_path(),
        ) {
            let folder_url = join_url(site.mount.as_str(), folder_key.as_str());
            let synthetic_title = folder_key
                .rsplit('/')
                .next()
                .map(humanize)
                .unwrap_or_else(|| site.title.clone());
            let crumbs = breadcrumbs_for_index_path(
                site.title.as_str(),
                site.mount.as_str(),
                view.pages_map(),
                folder_key.as_str(),
                synthetic_title.as_str(),
            );
            let ctx = PageTemplateContext {
                site: SiteContext {
                    title: site.title.clone(),
                    mount: site.mount.clone(),
                    root_url: site.mount.clone(),
                    color: site.color.clone(),
                },
                page: PageContext {
                    title: synthetic_title,
                    description: Some(format!(
                        "Auto-generated listing of pages under `{}`.",
                        folder_key
                    )),
                    url: folder_url.clone(),
                    url_path: folder_key.clone(),
                    layout: "doc".to_string(),
                    draft: false,
                    headings: vec![],
                    frontmatter: serde_json::json!({}),
                    html: String::new(),
                },
                nav_flat: view.nav_flat(),
                breadcrumbs: crumbs,
                prev: None,
                next: None,
                site_index: Some(folder_site_index),
                all_sites: all_sites.to_vec(),
                config: config_summary(state),
                live_reload: state.live_reload_enabled,
            };
            let mut html = {
                let renderer = state.renderer.read().await;
                renderer.render_page(&ctx).map_err(AppError::from)?
            };
            html = inject_live_reload(html, state.live_reload_enabled);
            log_access(state, viewer, request_path, Outcome::Allow);
            return Ok(Html(html).into_response());
        }
    }

    // US-18/SEC-8. Attachments and raw files bypass page rendering entirely, so the
    // check has to be repeated here. A gated page whose images load anyway is not a
    // gated page.
    if let Some((file_path, rel_path)) = resolve_static_file(&site, &normalized) {
        if !site.allows_path(&rel_path, viewer.email()) {
            if let Some(response) = deny(state) {
                log_access(state, viewer, request_path, Outcome::Deny);
                return Ok(response);
            }
            return Err(AppError::not_found("not found"));
        }
        let bytes = tokio::fs::read(&file_path)
            .await
            .map_err(|e| AppError::from(anyhow::anyhow!(e)))?;
        let mime = content_type_for_path(&file_path);
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
        log_access(state, viewer, request_path, Outcome::Allow);
        return Ok((headers, bytes).into_response());
    }

    // Nothing matched. For a signed-in viewer this is the same answer as "you may not
    // read that", which is the whole of D23.
    if let Some(response) = deny(state) {
        log_access(state, viewer, request_path, Outcome::Deny);
        return Ok(response);
    }

    let not_found_message = format!("No page or file at `{}`.", tail);
    let err_html = render_error_page(state, 404, "Not found", &not_found_message).await?;
    Ok((StatusCode::NOT_FOUND, Html(err_html)).into_response())
}

fn resolve_markdown_page<'a>(view: &'a SiteView, normalized: &str) -> Option<&'a Page> {
    let rel = normalized.trim_matches('/');
    if rel.is_empty() {
        return view.page("");
    }
    let rel_lower = rel.to_ascii_lowercase();
    if rel_lower.ends_with(".md") {
        let key = strip_md_url_suffix(rel);
        return view.page(&key);
    }
    if let Some(page) = view.page(rel) {
        return Some(page);
    }
    let with_index = format!("{}/index", rel);
    view.page(&with_index)
}

fn strip_md_url_suffix(rel: &str) -> String {
    rel.rsplit_once('.')
        .filter(|(_, ext)| ext.eq_ignore_ascii_case("md"))
        .map(|(base, _)| base.to_string())
        .unwrap_or_else(|| rel.to_string())
}

/// Resolve a static file, returning its absolute path and the site-relative path the
/// ACL must be evaluated against.
///
/// The returned relative path comes from the *canonicalized* file, not from the request
/// string. On a case-insensitive filesystem (macOS, Windows) a request for
/// `HR/chart.png` opens `hr/chart.png` quite happily, and authorizing the request string
/// would look up a folder named `HR` — miss the rule on `hr`, and fall through to
/// whatever broader rule applies. Authorizing the path the filesystem actually resolved
/// closes that, and closes symlink escapes at the same time.
fn resolve_static_file(site: &Site, normalized: &str) -> Option<(PathBuf, PathBuf)> {
    let rel = safe_relative_path(normalized)?;
    let true_rel = true_relative_path(&site.root, &rel)?;
    let full = site.root.join(&true_rel);
    if !full.is_file() {
        return None;
    }
    Some((full, true_rel))
}

/// Resolve `rel` to the casing the filesystem actually uses, one component at a time.
///
/// Authorization has to be evaluated against the path the rules are keyed on. On a
/// case-insensitive filesystem a request for `HR/chart.png` opens `hr/chart.png`, so
/// authorizing the request string would look up a folder named `HR`, miss the rule on
/// `hr`, and fall through to whatever broader rule applies.
///
/// Deliberately *not* `canonicalize`. That resolves symlinks too, and mdshelf follows
/// symlinks on purpose — a vault assembled from linked note directories is a supported
/// shape. Canonicalising put such files outside the site root and 404'd them, which both
/// broke that shape and made pages and their images disagree. Walking the components
/// fixes the casing without ever leaving the vault's own view of itself.
fn true_relative_path(root: &Path, rel: &Path) -> Option<PathBuf> {
    let mut current = root.to_path_buf();
    let mut resolved = PathBuf::new();

    for component in rel.components() {
        let requested = component.as_os_str();

        // The directory listing is the only trustworthy source of the real name.
        //
        // `current.join(requested).exists()` is not: on a case-insensitive filesystem it
        // answers true for `HR` when only `hr` exists, which is exactly the case this
        // function exists to correct. Prefer an exact entry — the only right answer on a
        // case-sensitive filesystem, where `HR` and `hr` are different directories — and
        // fall back to a case-insensitive one, which can only match on a filesystem that
        // would have opened it anyway.
        let requested_str = requested.to_str()?;
        let mut exact = None;
        let mut insensitive = None;
        for entry in std::fs::read_dir(&current).ok()?.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if name == requested_str {
                exact = Some(file_name);
                break;
            }
            if insensitive.is_none() && name.eq_ignore_ascii_case(requested_str) {
                insensitive = Some(file_name);
            }
        }
        let name = exact.or(insensitive)?;

        current = current.join(&name);
        resolved.push(&name);
    }

    Some(resolved)
}

fn normalize_tail(tail: &str) -> String {
    percent_encoding::percent_decode_str(tail)
        .decode_utf8_lossy()
        .trim_start_matches('/')
        .to_string()
}

fn safe_relative_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let mut out = PathBuf::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return None;
        }
        out.push(segment);
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// The site switcher shown on every page.
///
/// `page_count` is computed from the viewer's own projection: the raw total would tell
/// a reader how many pages exist in a site they can only partly see, which is exactly
/// the kind of structural detail D11 keeps confidential. Sites with nothing visible are
/// omitted entirely.
fn site_list_entries_for(universe: &Universe, viewer: Option<&str>) -> Vec<SiteListEntry> {
    universe
        .sites()
        .iter()
        .filter_map(|site| {
            let page_count = site.view(viewer).pages().count();
            if viewer.is_some() && page_count == 0 {
                return None;
            }
            Some(SiteListEntry {
                title: site.title.clone(),
                mount: site.mount.clone(),
                url: site.mount.clone(),
                page_count,
                color: site.color.clone(),
            })
        })
        .collect()
}

fn site_list_entries(universe: &Universe) -> Vec<SiteListEntry> {
    site_list_entries_for(universe, None)
}

fn config_summary(state: &AppState) -> ConfigSummary {
    ConfigSummary {
        version: env!("CARGO_PKG_VERSION").to_string(),
        theme_name: state.config.theme.name.clone(),
    }
}

async fn render_error_page(
    state: &AppState,
    status: u16,
    title: &str,
    message: &str,
) -> Result<String, AppError> {
    let all_sites = {
        let universe = state.universe.read().await;
        site_list_entries(&universe)
    };
    let ctx = ErrorTemplateContext {
        status,
        title: title.to_string(),
        message: message.to_string(),
        all_sites,
        config: config_summary(state),
        live_reload: state.live_reload_enabled,
    };
    let mut html = {
        let renderer = state.renderer.read().await;
        renderer.render_error(&ctx).map_err(AppError::from)?
    };
    html = inject_live_reload(html, state.live_reload_enabled);
    Ok(html)
}

fn inject_live_reload(html: String, enabled: bool) -> String {
    if !enabled {
        return html;
    }
    let script = r#"<script src="/__assets/js/livereload.js" defer></script>"#;
    if let Some(position) = html.rfind("</body>") {
        let mut output = String::with_capacity(html.len() + script.len());
        output.push_str(&html[..position]);
        output.push_str(script);
        output.push_str(&html[position..]);
        output
    } else {
        format!("{}\n{}", html, script)
    }
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("ico") => "image/x-icon",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
