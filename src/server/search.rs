//! `GET /__search` (the results page) and `GET /__search.json` (the search dialog).
//!
//! Registered only when `[search]` is configured. Both answer from [`visible_results`],
//! which is the single place a ranked file becomes a result: through the reader's own
//! projection of the site, exactly as a page request resolves a URL.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Query, State},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};

use crate::content::page::humanize;
use crate::content::{Site, SiteView};
use crate::render::templates::{SearchResultContext, SearchTemplateContext};
use crate::search::{self, DIALOG_RESULTS, PAGE_RESULTS, RankedFile};
use crate::server::AppState;
use crate::server::error::AppError;
use crate::server::routes::{
    Viewer, config_summary, interstitial_response, resolve_viewer, site_list_entries_for,
};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/__search", get(results_page))
        .route("/__search.json", get(results_json))
}

#[derive(Deserialize)]
struct Params {
    #[serde(default)]
    q: String,
}

/// What a search produced for one reader.
enum Outcome {
    Results(Vec<SearchResultContext>),
    /// qmd could not be reached or answered with an error.
    Unavailable,
}

async fn results_page(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Query(params): Query<Params>,
) -> Result<Response, AppError> {
    let viewer = resolve_viewer(&state, &jar).await;
    if matches!(viewer, Viewer::Anonymous) {
        return Ok(interstitial_response(&state));
    }
    let query = search::normalize_query(&params.q);
    let outcome = run(&state, &viewer, &query, PAGE_RESULTS).await;
    let all_sites = {
        let universe = state.universe.read().await;
        site_list_entries_for(&universe, viewer.email())
    };
    let (results, unavailable) = match outcome {
        Outcome::Results(results) => (results, false),
        Outcome::Unavailable => (Vec::new(), true),
    };
    let ctx = SearchTemplateContext {
        query,
        results,
        unavailable,
        all_sites,
        config: config_summary(&state),
        live_reload: false,
    };
    let html = state
        .renderer
        .read()
        .await
        .render_search(&ctx)
        .map_err(AppError::from)?;
    Ok(Html(html).into_response())
}

#[derive(Serialize)]
struct JsonAnswer {
    query: String,
    results: Vec<SearchResultContext>,
    unavailable: bool,
}

async fn results_json(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Query(params): Query<Params>,
) -> Response {
    let viewer = resolve_viewer(&state, &jar).await;
    if matches!(viewer, Viewer::Anonymous) {
        return interstitial_response(&state);
    }
    let query = search::normalize_query(&params.q);
    let (results, unavailable) = match run(&state, &viewer, &query, DIALOG_RESULTS).await {
        Outcome::Results(results) => (results, false),
        Outcome::Unavailable => (Vec::new(), true),
    };
    Json(JsonAnswer {
        query,
        results,
        unavailable,
    })
    .into_response()
}

async fn run(state: &AppState, viewer: &Viewer, query: &str, limit: usize) -> Outcome {
    let Some(runtime) = state.search.as_ref() else {
        return Outcome::Results(Vec::new());
    };
    if query.is_empty() {
        return Outcome::Results(Vec::new());
    }
    let ranked = match runtime.rank(query).await {
        Ok(ranked) => ranked,
        Err(error) => {
            // The query is left out of the log line: it is the reader's, not ours.
            tracing::warn!(error = %format!("{error:#}"), "search index unavailable");
            return Outcome::Unavailable;
        }
    };
    let universe = state.universe.read().await;
    Outcome::Results(visible_results(
        universe.sites(),
        viewer.email(),
        &ranked,
        query,
        limit,
    ))
}

/// The ranked files `viewer` may open, as results, in rank order.
///
/// A file that maps to no page, a page outside the reader's projection and a draft are
/// all skipped the same way and leave no trace in the answer, so a query that ranks
/// only restricted pages answers exactly like one that ranks nothing.
fn visible_results(
    sites: &[Arc<Site>],
    viewer: Option<&str>,
    ranked: &[RankedFile],
    query: &str,
    limit: usize,
) -> Vec<SearchResultContext> {
    let by_collection: HashMap<String, &Arc<Site>> = sites
        .iter()
        .map(|site| (search::collection_name(&site.mount), site))
        .collect();
    let mut views: HashMap<&str, Arc<SiteView>> = HashMap::new();
    let mut results = Vec::new();
    for file in ranked {
        if results.len() >= limit {
            break;
        }
        let Some(site) = by_collection.get(&file.collection) else {
            continue;
        };
        let Some(url_path) = file
            .source_candidates()
            .into_iter()
            .find_map(|candidate| site.url_path_for(candidate))
        else {
            continue;
        };
        let view = views
            .entry(site.mount.as_str())
            .or_insert_with(|| site.view(viewer));
        let Some(page) = view.page(url_path) else {
            continue;
        };
        if page.draft
            || results
                .iter()
                .any(|r: &SearchResultContext| r.url == page.url)
        {
            continue;
        }
        results.push(SearchResultContext {
            title: page.title.clone(),
            url: page.url.clone(),
            site: site.title.clone(),
            color: site.color.clone(),
            section: section_of(&page.url_path),
            snippet: search::snippet(page, query),
        });
    }
    results
}

/// The folders above a page, humanised and joined: `legal/policies/privacy` →
/// "Legal / Policies".
fn section_of(url_path: &str) -> String {
    let mut parts: Vec<&str> = url_path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    parts.pop();
    parts
        .into_iter()
        .map(humanize)
        .collect::<Vec<_>>()
        .join(" / ")
}
