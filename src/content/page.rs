use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gray_matter::{Matter, ParsedEntity, engine::YAML};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::render::markdown::{Heading, LinkCtx, MarkdownRenderer};

/// One rendered Markdown page within a site.
#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub fs_path: PathBuf,
    pub rel_path: PathBuf,
    /// URL path within the site (no leading slash, no extension). Empty for the root index.
    pub url_path: String,
    /// Absolute URL with the site's mount prefix (e.g. "/notes/guide/intro").
    pub url: String,
    pub is_index: bool,
    pub title: String,
    pub description: Option<String>,
    pub layout: String,
    pub sidebar_order: Option<i64>,
    pub draft: bool,
    pub frontmatter: JsonValue,
    pub headings: Vec<Heading>,
    pub html: String,
}

#[derive(Debug, Default, Deserialize)]
struct FrontmatterFields {
    title: Option<String>,
    description: Option<String>,
    layout: Option<String>,
    sidebar_order: Option<i64>,
    #[serde(default)]
    draft: bool,
}

impl Page {
    pub fn load(
        site_root: &Path,
        rel_path: &Path,
        mount: &str,
        renderer: &MarkdownRenderer,
    ) -> Result<Page> {
        let fs_path = site_root.join(rel_path);
        let raw = std::fs::read_to_string(&fs_path)
            .with_context(|| format!("reading {}", fs_path.display()))?;

        let matter = Matter::<YAML>::new();
        let parsed: ParsedEntity<JsonValue> = matter
            .parse(&raw)
            .map_err(|err| anyhow::anyhow!("front matter in {}: {}", fs_path.display(), err))?;
        let body = parsed.content;
        let frontmatter_value = parsed.data.unwrap_or(JsonValue::Object(Default::default()));
        let typed: FrontmatterFields =
            serde_json::from_value(frontmatter_value.clone()).unwrap_or_default();

        let (url_path, is_index) = url_path_from_rel(rel_path);
        let url = join_url(mount, &url_path);

        let link_ctx = LinkCtx {
            mount: mount.to_string(),
            current_dir: rel_path.parent().map(Path::to_path_buf).unwrap_or_default(),
        };
        let rendered = renderer.render(&body, &link_ctx);

        let title = typed
            .title
            .clone()
            .or_else(|| {
                rendered
                    .headings
                    .iter()
                    .find(|h| h.level == 1)
                    .map(|h| h.text.clone())
            })
            .unwrap_or_else(|| derive_title_from_path(rel_path));
        let layout = typed.layout.unwrap_or_else(|| "doc".to_string());

        Ok(Page {
            fs_path,
            rel_path: rel_path.to_path_buf(),
            url_path,
            url,
            is_index,
            title,
            description: typed.description,
            layout,
            sidebar_order: typed.sidebar_order,
            draft: typed.draft,
            frontmatter: frontmatter_value,
            html: rendered.html,
            headings: rendered.headings,
        })
    }
}

/// Translate a relative file path inside a site root into a clean URL path
/// (without leading slash and without `.md` extension).
pub fn url_path_from_rel(rel: &Path) -> (String, bool) {
    let mut parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let last = parts.pop().unwrap_or_default();
    let stem = last.strip_suffix(".md").unwrap_or(&last);
    let is_index = stem.eq_ignore_ascii_case("index");
    if !is_index {
        parts.push(stem.to_string());
    }
    (parts.join("/"), is_index)
}

/// Compose the absolute URL of a page from its mount and url_path.
pub fn join_url(mount: &str, url_path: &str) -> String {
    let m = if mount == "/" { "" } else { mount };
    if url_path.is_empty() {
        if m.is_empty() {
            "/".to_string()
        } else {
            m.to_string()
        }
    } else {
        format!("{}/{}", m, url_path)
    }
}

fn derive_title_from_path(rel: &Path) -> String {
    let stem = rel.file_stem().map(|s| s.to_string_lossy().into_owned());
    match stem {
        Some(name) if name.eq_ignore_ascii_case("index") => rel
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| humanize(&s.to_string_lossy()))
            .unwrap_or_else(|| "Home".to_string()),
        Some(name) => humanize(&name),
        None => "Untitled".to_string(),
    }
}

/// "getting-started" -> "Getting Started", "01-intro" -> "Intro".
pub fn humanize(slug: &str) -> String {
    let cleaned = slug.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-' || c == '_');
    let cleaned = if cleaned.is_empty() { slug } else { cleaned };
    let mut out = String::with_capacity(cleaned.len());
    for (idx, word) in cleaned
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .enumerate()
    {
        if idx > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.extend(chars);
        }
    }
    if out.is_empty() {
        slug.to_string()
    } else {
        out
    }
}
