use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::content::Page;
use crate::content::page::humanize;

/// Rows for an auto-generated index (site root or a folder without `index.md`).
#[derive(Debug, Clone, Serialize)]
pub struct SiteIndexContext {
    pub rows: Vec<SiteIndexRow>,
    pub content_root_display: String,
    pub lead: String,
    pub hint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SiteIndexRow {
    pub depth: u32,
    pub is_folder_heading: bool,
    pub title: String,
    pub description: Option<String>,
    pub url: Option<String>,
    pub path_label: String,
}

pub fn build_site_index_context(
    pages: &BTreeMap<String, Page>,
    site_content_root: &Path,
) -> SiteIndexContext {
    let root_display = site_content_root.display().to_string();
    let lead = "Every page in this site is listed below, grouped by folder. Page titles and descriptions come from front matter or the first heading in each file when available.".to_string();
    let hint = format!(
        "To replace this page, add `index.md` in `{}`. URLs omit the `.md` suffix.",
        root_display
    );
    let mut emitted_folder_prefixes = BTreeSet::new();
    let rows = build_site_index_rows(pages, "", &mut emitted_folder_prefixes);
    SiteIndexContext {
        rows,
        content_root_display: root_display,
        lead,
        hint,
    }
}

/// Listing for `url_prefix` (e.g. `docs/decisions`) when there is no page at that path but
/// published pages exist under `url_prefix/…`.
pub fn build_site_index_under_prefix(
    pages: &BTreeMap<String, Page>,
    url_prefix: &str,
    site_content_root: &Path,
) -> Option<SiteIndexContext> {
    let prefix = url_prefix.trim_matches('/');
    if prefix.is_empty() {
        return None;
    }
    if pages.contains_key(prefix) {
        return None;
    }
    let needle = format!("{}/", prefix);
    let has_descendant = pages
        .values()
        .any(|page| !page.draft && page.url_path.starts_with(needle.as_str()));
    if !has_descendant {
        return None;
    }
    let mut emitted_folder_prefixes: BTreeSet<String> = BTreeSet::new();
    let rows = build_site_index_rows(pages, prefix, &mut emitted_folder_prefixes);
    let fs_dir = prefix
        .split('/')
        .filter(|segment| !segment.is_empty())
        .fold(site_content_root.to_path_buf(), |accumulated, segment| {
            accumulated.join(segment)
        });
    let lead = format!(
        "Every published page under `{}` in this site is listed below.",
        prefix
    );
    let hint = format!(
        "To replace this page, add `index.md` in `{}`. URLs omit the `.md` suffix.",
        fs_dir.display()
    );
    Some(SiteIndexContext {
        rows,
        content_root_display: site_content_root.display().to_string(),
        lead,
        hint,
    })
}

fn build_site_index_rows(
    pages: &BTreeMap<String, Page>,
    path_prefix: &str,
    emitted_folder_prefixes: &mut BTreeSet<String>,
) -> Vec<SiteIndexRow> {
    let mut rows = Vec::new();
    let mut page_refs: Vec<&Page> = pages
        .values()
        .filter(|page| !page.draft)
        .filter(|page| {
            if path_prefix.is_empty() {
                return !page.url_path.is_empty();
            }
            let needle = format!("{}/", path_prefix);
            page.url_path.starts_with(needle.as_str())
        })
        .collect();
    page_refs.sort_by(|left, right| {
        left.url_path
            .cmp(&right.url_path)
            .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
    });

    for page in page_refs {
        if path_prefix.is_empty() && page.url_path.is_empty() {
            continue;
        }

        let relative = if path_prefix.is_empty() {
            page.url_path.as_str()
        } else {
            let needle = format!("{}/", path_prefix);
            let Some(rest) = page.url_path.strip_prefix(needle.as_str()) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            rest
        };

        let segments: Vec<&str> = relative
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();

        for prefix_depth in 0..segments.len().saturating_sub(1) {
            let relative_folder = segments[..=prefix_depth].join("/");
            let full_folder_key = if path_prefix.is_empty() {
                relative_folder.clone()
            } else {
                format!("{}/{}", path_prefix, relative_folder)
            };
            if !emitted_folder_prefixes.insert(full_folder_key.clone()) {
                continue;
            }
            let folder_slug = segments[prefix_depth];
            let index_page = pages.get(&full_folder_key);
            let title = index_page
                .map(|index| index.title.clone())
                .unwrap_or_else(|| humanize(folder_slug));
            let description = index_page.and_then(|index| index.description.clone());
            let url = index_page.map(|index| index.url.clone());
            rows.push(SiteIndexRow {
                depth: prefix_depth as u32,
                is_folder_heading: true,
                title,
                description,
                url,
                path_label: full_folder_key,
            });
        }

        let skip_duplicate_index_row = page.is_index && segments.len() > 1;
        if skip_duplicate_index_row {
            continue;
        }

        let depth = segments.len().saturating_sub(1) as u32;
        rows.push(SiteIndexRow {
            depth,
            is_folder_heading: false,
            title: page.title.clone(),
            description: page.description.clone(),
            url: Some(page.url.clone()),
            path_label: page.url_path.clone(),
        });
    }

    rows
}
