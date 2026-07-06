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

use crate::content::page::{humanize, join_url};
use crate::content::tree::{breadcrumbs, breadcrumbs_for_index_path, prev_next};
use crate::content::{
    Page, Site, Universe, build_site_index_context, build_site_index_under_prefix,
};
use crate::render::templates::{
    ConfigSummary, Crumb, ErrorTemplateContext, HomeTemplateContext, NeighborContext, PageContext,
    PageTemplateContext, SiteContext, SiteListEntry,
};
use crate::server::AppState;
use crate::server::error::AppError;
use crate::server::livereload;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/__mdshelf/syntax.css", get(syntax_css))
        .route("/__assets/{*asset_path}", get(theme_asset))
        .route("/__livereload", get(livereload::livereload_ws))
        .route("/{*rest}", get(site_or_not_found))
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_http())
                .layer(CompressionLayer::new()),
        )
        .with_state(state)
}

async fn home(State(state): State<Arc<AppState>>) -> Result<Response, AppError> {
    let all_sites = {
        let universe = state.universe.read().await;
        site_list_entries(&universe)
    };
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

async fn site_or_not_found(
    State(state): State<Arc<AppState>>,
    uri: Uri,
) -> Result<Response, AppError> {
    let raw_path = uri.path();
    if raw_path.starts_with("/__") {
        return Err(AppError::not_found("not found"));
    }
    let path = raw_path.trim_end_matches('/');
    let matched = {
        let universe = state.universe.read().await;
        match_site_path(&universe, path)
    };
    let Some((site, tail, all_sites)) = matched else {
        let body = render_error_page(
            &state,
            404,
            "Not found",
            "No configured site matches this URL.",
        )
        .await?;
        return Ok((StatusCode::NOT_FOUND, Html(body)).into_response());
    };
    serve_site_request(&state, site, &tail, &all_sites).await
}

fn match_site_path(
    universe: &Universe,
    request_path: &str,
) -> Option<(Arc<Site>, String, Vec<SiteListEntry>)> {
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
    let all_sites = site_list_entries(universe);
    Some((site, tail.to_string(), all_sites))
}

async fn serve_site_request(
    state: &Arc<AppState>,
    site: Arc<Site>,
    tail: &str,
    all_sites: &[SiteListEntry],
) -> Result<Response, AppError> {
    let normalized = normalize_tail(tail);

    if normalized.trim().is_empty() && site.page("").is_none() {
        let site_index = build_site_index_context(site.pages_map(), site.root.as_path());
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
            nav_flat: site.nav_flat(),
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

    if let Some(page) = resolve_markdown_page(&site, &normalized) {
        if page.draft {
            return Err(AppError::not_found("draft"));
        }
        let crumbs = breadcrumbs(
            site.title.as_str(),
            site.mount.as_str(),
            site.pages_map(),
            page,
        );
        let (prev_page, next_page) = prev_next(site.pages_map(), page);
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
            nav_flat: site.nav_flat(),
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
            site.pages_map(),
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
                site.pages_map(),
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
                nav_flat: site.nav_flat(),
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
            return Ok(Html(html).into_response());
        }
    }

    if let Some(file_path) = resolve_static_file(&site, &normalized) {
        let bytes = tokio::fs::read(&file_path)
            .await
            .map_err(|e| AppError::from(anyhow::anyhow!(e)))?;
        let mime = content_type_for_path(&file_path);
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
        return Ok((headers, bytes).into_response());
    }

    let not_found_message = format!("No page or file at `{}`.", tail);
    let err_html = render_error_page(state, 404, "Not found", &not_found_message).await?;
    Ok((StatusCode::NOT_FOUND, Html(err_html)).into_response())
}

fn resolve_markdown_page<'a>(site: &'a Site, normalized: &str) -> Option<&'a Page> {
    let rel = normalized.trim_matches('/');
    if rel.is_empty() {
        return site.page("");
    }
    let rel_lower = rel.to_ascii_lowercase();
    if rel_lower.ends_with(".md") {
        let key = strip_md_url_suffix(rel);
        return site.page(&key);
    }
    if let Some(page) = site.page(rel) {
        return Some(page);
    }
    let with_index = format!("{}/index", rel);
    site.page(&with_index)
}

fn strip_md_url_suffix(rel: &str) -> String {
    rel.rsplit_once('.')
        .filter(|(_, ext)| ext.eq_ignore_ascii_case("md"))
        .map(|(base, _)| base.to_string())
        .unwrap_or_else(|| rel.to_string())
}

fn resolve_static_file(site: &Site, normalized: &str) -> Option<PathBuf> {
    let rel = safe_relative_path(normalized)?;
    let full = site.root.join(&rel);
    if full.is_file() {
        return Some(full);
    }
    None
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

fn site_list_entries(universe: &Universe) -> Vec<SiteListEntry> {
    universe
        .sites()
        .iter()
        .map(|site| SiteListEntry {
            title: site.title.clone(),
            mount: site.mount.clone(),
            url: site.mount.clone(),
            page_count: site.pages().count(),
            color: site.color.clone(),
        })
        .collect()
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
