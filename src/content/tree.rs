use std::collections::BTreeMap;

use serde::Serialize;

use crate::content::page::{Page, humanize, join_url};

const INDEX_SLUG: &str = "__index__";

/// One row in the sidebar after depth-first flattening (arbitrary tree depth).
#[derive(Debug, Clone, Serialize)]
pub struct SidebarNavRow {
    pub depth: u32,
    pub title: String,
    pub filename: String,
    pub modified_at_ms: i64,
    /// Stable key for folder collapse state and client-side identity.
    pub stable_key: String,
    pub url: Option<String>,
    pub is_index: bool,
}

/// Flattens the nav tree for templates that cannot recurse (sidebar lists every depth).
pub fn flatten_nav_sidebar_rows(
    nav_root: &NavNode,
    pages: &BTreeMap<String, Page>,
) -> Vec<SidebarNavRow> {
    let mut rows = Vec::new();
    for child in &nav_root.children {
        flatten_nav_depth_first(child, 0, pages, &mut rows);
    }
    rows
}

fn flatten_nav_depth_first(
    node: &NavNode,
    depth: u32,
    pages: &BTreeMap<String, Page>,
    rows: &mut Vec<SidebarNavRow>,
) {
    let (filename, modified_at_ms) = sidebar_row_metadata(node, pages);
    rows.push(SidebarNavRow {
        depth,
        title: node.title.clone(),
        filename,
        modified_at_ms,
        stable_key: sidebar_stable_key(node),
        url: node.url.clone(),
        is_index: node.slug == INDEX_SLUG,
    });
    for child in &node.children {
        flatten_nav_depth_first(child, depth + 1, pages, rows);
    }
}

fn sidebar_stable_key(node: &NavNode) -> String {
    node.url_path
        .clone()
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| node.slug.clone())
}

fn sidebar_row_metadata(node: &NavNode, pages: &BTreeMap<String, Page>) -> (String, i64) {
    if node.url.is_some() {
        if let Some(url_path) = node.url_path.as_deref() {
            if let Some(page) = pages.get(url_path) {
                let filename = if node.slug == INDEX_SLUG {
                    index_row_filename(url_path)
                } else {
                    page.filename.clone()
                };
                return (filename, page.modified_at_ms);
            }
        }
    }
    let filename = if node.slug.is_empty() {
        node.title.clone()
    } else {
        node.slug.clone()
    };
    let modified_at_ms = max_modified_at_ms(node, pages);
    (filename, modified_at_ms)
}

/// Label for folder `index.md` rows in filename mode (not the literal `index.md` basename).
fn index_row_filename(url_path: &str) -> String {
    if url_path.is_empty() {
        "index.md".to_string()
    } else {
        url_path
            .rsplit('/')
            .next()
            .unwrap_or(url_path)
            .to_string()
    }
}

fn max_modified_at_ms(node: &NavNode, pages: &BTreeMap<String, Page>) -> i64 {
    let mut max_ms = 0_i64;
    if let Some(url_path) = node.url_path.as_deref() {
        if let Some(page) = pages.get(url_path) {
            max_ms = max_ms.max(page.modified_at_ms);
        }
    }
    for child in &node.children {
        max_ms = max_ms.max(max_modified_at_ms(child, pages));
    }
    max_ms
}

/// A node in the recursive sidebar navigation tree. Each node is a section
/// (folder) or a leaf page. Folders with `index.md` expose that page as a
/// pinned `__index__` child rather than attaching the URL to the folder node.
#[derive(Debug, Clone, Serialize)]
pub struct NavNode {
    pub slug: String,
    pub title: String,
    pub url: Option<String>,
    pub url_path: Option<String>,
    pub order: i64,
    pub depth: u32,
    pub children: Vec<NavNode>,
}

const DEFAULT_ORDER: i64 = 1_000_000;

impl NavNode {
    fn section(slug: String, title: String, url_path: String, depth: u32) -> Self {
        Self {
            slug,
            title,
            url: None,
            url_path: Some(url_path),
            order: DEFAULT_ORDER,
            depth,
            children: Vec::new(),
        }
    }

    pub fn build(site_title: &str, mount: &str, pages: &BTreeMap<String, Page>) -> Self {
        let mut root = Self::section(String::new(), site_title.to_string(), String::new(), 0);

        for page in pages.values() {
            if page.draft {
                continue;
            }
            let segments: Vec<&str> = if page.url_path.is_empty() {
                Vec::new()
            } else {
                page.url_path.split('/').collect()
            };
            insert(&mut root, &segments, page, mount);
        }
        sort_recursive(&mut root);
        root
    }
}

fn insert(node: &mut NavNode, segments: &[&str], page: &Page, mount: &str) {
    if segments.is_empty() {
        // Root-level index.md has no parent folder to attach to; skip.
        if node.url_path.as_deref().map_or(true, |p| p.is_empty()) {
            return;
        }
        // Folder index.md: insert as a pinned first child so the folder
        // itself stays a plain collapsible section with no navigation URL.
        if !node.children.iter().any(|c| c.slug == INDEX_SLUG) {
            node.children.push(NavNode {
                slug: INDEX_SLUG.to_string(),
                title: page.title.clone(),
                url: Some(page.url.clone()),
                url_path: Some(page.url_path.clone()),
                order: page.sidebar_order.unwrap_or(DEFAULT_ORDER),
                depth: node.depth + 1,
                children: Vec::new(),
            });
        }
        return;
    }

    let head = segments[0];
    let rest = &segments[1..];

    let position = node.children.iter().position(|c| c.slug == head);
    let index = match position {
        Some(idx) => idx,
        None => {
            let url_path = match &node.url_path {
                Some(p) if !p.is_empty() => format!("{}/{}", p, head),
                _ => head.to_string(),
            };
            let depth = node.depth + 1;
            node.children.push(NavNode::section(
                head.to_string(),
                humanize(head),
                url_path,
                depth,
            ));
            node.children.len() - 1
        }
    };

    if rest.is_empty() && !page.is_index {
        let child = &mut node.children[index];
        child.title = page.title.clone();
        child.url = Some(page.url.clone());
        child.url_path = Some(page.url_path.clone());
        child.order = page.sidebar_order.unwrap_or(DEFAULT_ORDER);
        let _ = mount;
        return;
    }

    insert(&mut node.children[index], rest, page, mount);
}

fn sort_recursive(node: &mut NavNode) {
    node.children.sort_by(|a, b| {
        let a_is_index = a.slug == INDEX_SLUG;
        let b_is_index = b.slug == INDEX_SLUG;
        let a_is_folder = !a.children.is_empty();
        let b_is_folder = !b.children.is_empty();
        b_is_folder
            .cmp(&a_is_folder)
            .then_with(|| b_is_index.cmp(&a_is_index))
            .then_with(|| a.order.cmp(&b.order))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    for child in &mut node.children {
        sort_recursive(child);
    }
}

/// Produce breadcrumb entries from site root to the current page.
pub fn breadcrumbs(
    site_title: &str,
    mount: &str,
    pages: &BTreeMap<String, Page>,
    current: &Page,
) -> Vec<crate::render::templates::Crumb> {
    let mut out = Vec::new();
    out.push(crate::render::templates::Crumb {
        title: site_title.to_string(),
        url: join_url(mount, ""),
    });
    if current.url_path.is_empty() {
        return out;
    }
    let segments: Vec<&str> = current.url_path.split('/').collect();
    let mut accumulated = String::new();
    for (idx, seg) in segments.iter().enumerate() {
        if !accumulated.is_empty() {
            accumulated.push('/');
        }
        accumulated.push_str(seg);
        let is_last = idx + 1 == segments.len();
        let title = if is_last {
            current.title.clone()
        } else {
            pages
                .get(&accumulated)
                .map(|p| p.title.clone())
                .unwrap_or_else(|| humanize(seg))
        };
        out.push(crate::render::templates::Crumb {
            title,
            url: join_url(mount, &accumulated),
        });
    }
    out
}

/// Breadcrumbs along `url_path` (no trailing slash segments), with a fixed title for the last crumb.
/// Used for auto-generated folder index pages that are not backed by a [`Page`].
pub fn breadcrumbs_for_index_path(
    site_title: &str,
    mount: &str,
    pages: &BTreeMap<String, Page>,
    url_path: &str,
    terminal_title: &str,
) -> Vec<crate::render::templates::Crumb> {
    let mut out = Vec::new();
    out.push(crate::render::templates::Crumb {
        title: site_title.to_string(),
        url: join_url(mount, ""),
    });
    if url_path.trim().is_empty() {
        return out;
    }
    let segments: Vec<&str> = url_path.split('/').filter(|segment| !segment.is_empty()).collect();
    let mut accumulated = String::new();
    for (segment_index, segment) in segments.iter().enumerate() {
        if !accumulated.is_empty() {
            accumulated.push('/');
        }
        accumulated.push_str(segment);
        let is_last = segment_index + 1 == segments.len();
        let title = if is_last {
            terminal_title.to_string()
        } else {
            pages
                .get(&accumulated)
                .map(|page| page.title.clone())
                .unwrap_or_else(|| humanize(segment))
        };
        out.push(crate::render::templates::Crumb {
            title,
            url: join_url(mount, &accumulated),
        });
    }
    out
}

/// Find the previous and next pages within the same parent section, ordered
/// by `sidebar_order` (then alphabetical). Used for prev/next pagination.
pub fn prev_next<'a>(
    pages: &'a BTreeMap<String, Page>,
    current: &'a Page,
) -> (Option<&'a Page>, Option<&'a Page>) {
    let parent = parent_path(&current.url_path);
    let mut siblings: Vec<&Page> = pages
        .values()
        .filter(|p| !p.draft)
        .filter(|p| parent_path(&p.url_path) == parent)
        .collect();
    siblings.sort_by(|a, b| {
        a.sidebar_order
            .unwrap_or(DEFAULT_ORDER)
            .cmp(&b.sidebar_order.unwrap_or(DEFAULT_ORDER))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    let pos = siblings
        .iter()
        .position(|p| p.url_path == current.url_path);
    match pos {
        Some(idx) => (
            idx.checked_sub(1).and_then(|i| siblings.get(i).copied()),
            siblings.get(idx + 1).copied(),
        ),
        None => (None, None),
    }
}

fn parent_path(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => path[..idx].to_string(),
        None => String::new(),
    }
}
