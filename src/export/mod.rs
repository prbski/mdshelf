use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};

use crate::cli::ExportArgs;
use crate::config::Config;
use crate::content::page::{escape_for_script_block, humanize, join_url};
use crate::content::source::iter_site_static_files;
use crate::content::tree::{breadcrumbs, breadcrumbs_for_index_path, prev_next};
use crate::content::{
    Page, Site, SiteIndexContext, Universe, build_site_index_context, build_site_index_under_prefix,
};
use crate::render::markdown::MarkdownRenderer;
use crate::render::templates::{
    ConfigSummary, Crumb, HomeTemplateContext, NeighborContext, PageContext, PageTemplateContext,
    Renderer, SiteContext, SiteListEntry,
};
use crate::theme::ThemeStack;

const SYNTAX_CSS_FILE: &str = "syntax-highlight.css";
const SYNTAX_CSS_ASSET: &str = "assets/syntax-highlight.css";
const LIVERELOAD_ASSET: &str = "assets/js/livereload.js";

struct SiteExportContext<'a> {
    site: &'a Site,
    mount: String,
    /// The projection being exported. With `--as`, only what that viewer may read;
    /// otherwise the whole site (US-20).
    view: Arc<crate::content::SiteView>,
    viewer: Option<String>,
}

impl<'a> SiteExportContext<'a> {
    fn from_site(site: &'a Site, flat: bool, viewer: Option<&str>) -> Self {
        Self {
            view: site.view(viewer),
            viewer: viewer.map(str::to_string),
            site,
            mount: if flat {
                "/".to_string()
            } else {
                site.mount.clone()
            },
        }
    }

    fn viewer(&self) -> Option<&str> {
        self.viewer.as_deref()
    }

    fn pages(&self) -> impl Iterator<Item = &Page> {
        self.view.pages()
    }

    fn pages_map(&self) -> &std::collections::BTreeMap<String, Page> {
        self.view.pages_map()
    }

    fn nav_flat(&self) -> Arc<Vec<crate::content::SidebarNavRow>> {
        self.view.nav_flat()
    }

    fn flat(&self) -> bool {
        self.mount == "/"
    }

    fn remap_url(&self, url: &str) -> String {
        if !self.flat() || self.site.mount == "/" {
            return url.to_string();
        }
        remap_mount_url(url, self.site.mount.as_str(), self.mount.as_str())
    }

    fn page_url(&self, page: &Page) -> String {
        join_url(self.mount.as_str(), page.url_path.as_str())
    }
}

pub fn run(args: ExportArgs) -> Result<()> {
    let config = Config::load(args.config.as_deref())?;
    let theme = ThemeStack::from_config(&config)?;
    let renderer = Renderer::new(&theme)?;
    let markdown = MarkdownRenderer::new();
    let universe = Universe::build(&config)?;

    let selected = select_sites(&universe, &args.site)?;
    let export_all = args.site.is_empty();
    let flat_single = selected.len() == 1;

    let viewer = resolve_export_viewer(&selected, args.as_viewer.as_deref())?;

    let output = expand_output_directory(&args.output)?;
    prepare_output_directory(&output, args.force)?;

    let config_summary = ConfigSummary {
        version: env!("CARGO_PKG_VERSION").to_string(),
        theme_name: config.theme.name.clone(),
    };
    let all_sites = site_list_entries_for_export(&selected, flat_single, viewer.as_deref());

    write_theme_assets(&output, &theme, &markdown)?;

    let mut page_count = 0usize;
    if export_all {
        export_home_page(&output, &renderer, &all_sites, &config_summary)?;
        page_count += 1;
    }

    let mut skipped = 0usize;
    for site in &selected {
        let ctx = SiteExportContext::from_site(site, flat_single, viewer.as_deref());
        skipped += site.pages().count() - ctx.pages().count();
        page_count += export_site(&output, &renderer, &ctx, &all_sites, &config_summary)?;
        copy_site_static_files(&output, &ctx)?;
    }

    let site_label = if export_all {
        "all sites".to_string()
    } else if selected.len() == 1 {
        selected[0].title.clone()
    } else {
        format!("{} sites", selected.len())
    };
    match viewer.as_deref() {
        Some(email) => println!(
            "exported {} page(s) visible to {} from {} to {}",
            page_count,
            email,
            site_label,
            output.display()
        ),
        None => println!(
            "exported {} page(s) from {} to {}",
            page_count,
            site_label,
            output.display()
        ),
    }
    if skipped > 0 {
        // Never silently drop pages: the reader of this line is deciding whether the
        // bundle they are about to send is complete.
        println!("skipped {skipped} restricted page(s)");
    }
    Ok(())
}

/// Decide whose view is being exported, and refuse the dangerous default.
///
/// A static bundle carries no authentication at all. Exporting a vault that declares
/// access rules without saying whose view it is would flatten a private vault into
/// world-readable HTML — the one outcome this feature exists to prevent (US-20).
fn resolve_export_viewer(sites: &[Arc<Site>], requested: Option<&str>) -> Result<Option<String>> {
    if let Some(raw) = requested {
        let email = crate::auth::normalize_email(raw);
        if !crate::auth::is_valid_email(&email) {
            bail!("--as `{raw}` is not a valid email address");
        }
        return Ok(Some(email));
    }

    let ruled: Vec<&str> = sites
        .iter()
        .filter(|site| !site.acl().is_empty())
        .map(|site| site.title.as_str())
        .collect();

    if ruled.is_empty() {
        // No rules anywhere: this vault was already public, and export behaves exactly
        // as it always has.
        return Ok(None);
    }

    bail!(
        "this vault declares access rules ({}), and a static export has no authentication —\n  \
         every exported page would be world-readable.\n  \
         Re-run with --as <email> to export exactly what that person can see.",
        ruled.join(", ")
    )
}

fn select_sites(universe: &Universe, filters: &[String]) -> Result<Vec<Arc<Site>>> {
    if filters.is_empty() {
        return Ok(universe.sites().to_vec());
    }

    let mut selected = Vec::with_capacity(filters.len());
    let mut seen_mounts = BTreeSet::new();
    for filter in filters {
        let site = universe
            .sites()
            .iter()
            .find(|site| site_matches_filter(site, filter))
            .with_context(|| format!("no site matches {filter:?}"))?;
        if seen_mounts.insert(site.mount.clone()) {
            selected.push(Arc::clone(site));
        }
    }
    Ok(selected)
}

fn site_matches_filter(site: &Site, filter: &str) -> bool {
    let trimmed = filter.trim();
    if trimmed.is_empty() {
        return false;
    }
    site.mount == normalize_mount(trimmed) || site.title.eq_ignore_ascii_case(trimmed)
}

fn normalize_mount(mount: &str) -> String {
    let trimmed = mount.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_string();
    }
    format!("/{}", trimmed.trim_start_matches('/'))
}

fn remap_mount_url(url: &str, from_mount: &str, to_mount: &str) -> String {
    if from_mount == to_mount {
        return url.to_string();
    }
    let from = from_mount.trim_end_matches('/');
    if from.is_empty() {
        return url.to_string();
    }
    if url == from {
        return join_url(to_mount, "");
    }
    if let Some(rest) = url.strip_prefix(from) {
        let suffix = rest.trim_start_matches('/');
        return join_url(to_mount, suffix);
    }
    url.to_string()
}

fn expand_output_directory(directory: &Path) -> Result<PathBuf> {
    let as_str = directory
        .to_str()
        .with_context(|| format!("output directory must be UTF-8: {}", directory.display()))?;
    Ok(PathBuf::from(shellexpand::tilde(as_str).into_owned()))
}

fn prepare_output_directory(output: &Path, force: bool) -> Result<()> {
    if output.exists() {
        if !force {
            bail!(
                "{} already exists; pass --force to overwrite",
                output.display()
            );
        }
        std::fs::remove_dir_all(output)
            .with_context(|| format!("removing {}", output.display()))?;
    }
    std::fs::create_dir_all(output).with_context(|| format!("creating {}", output.display()))?;
    Ok(())
}

fn write_theme_assets(
    output: &Path,
    theme: &ThemeStack,
    markdown: &MarkdownRenderer,
) -> Result<()> {
    for (relative, asset) in theme.list_asset_paths() {
        if relative == LIVERELOAD_ASSET {
            continue;
        }
        let target = output.join(&relative);
        write_bytes(&target, &asset.bytes)?;
    }

    let syntax_target = output.join(SYNTAX_CSS_ASSET);
    write_bytes(&syntax_target, markdown.syntax_css().as_bytes())?;
    Ok(())
}

fn export_home_page(
    output: &Path,
    renderer: &Renderer,
    all_sites: &[SiteListEntry],
    config_summary: &ConfigSummary,
) -> Result<()> {
    let ctx = HomeTemplateContext {
        all_sites: all_sites.to_vec(),
        config: config_summary.clone(),
        live_reload: false,
    };
    let html = renderer.render_home(&ctx)?;
    let path = output.join("index.html");
    write_html(&path, output, &html)?;
    Ok(())
}

fn export_site(
    output: &Path,
    renderer: &Renderer,
    site_ctx: &SiteExportContext<'_>,
    all_sites: &[SiteListEntry],
    config_summary: &ConfigSummary,
) -> Result<usize> {
    let mut count = 0usize;

    // A site the recipient can see nothing of must contribute nothing at all — not even
    // an empty auto-generated index, whose path and title would disclose that the site
    // exists. The bundle is handed to one person; another client's project name has no
    // business in it.
    if site_ctx.viewer().is_some() && site_ctx.pages().next().is_none() {
        return Ok(0);
    }

    // Whether a root index needs synthesising is a question about *this viewer's* site,
    // not the whole one: a root `index.md` they may not read leaves them with no
    // landing page at all.
    if site_ctx.view.page("").is_none() {
        export_site_root_index(output, renderer, site_ctx, all_sites, config_summary)?;
        count += 1;
    }

    for page in site_ctx.pages() {
        if page.draft {
            continue;
        }
        export_page(
            output,
            renderer,
            site_ctx,
            page,
            all_sites,
            config_summary,
            None,
        )?;
        count += 1;
    }

    for folder_key in folder_index_keys(site_ctx) {
        export_folder_index(
            output,
            renderer,
            site_ctx,
            folder_key.as_str(),
            all_sites,
            config_summary,
        )?;
        count += 1;
    }

    Ok(count)
}

fn folder_index_keys(site_ctx: &SiteExportContext<'_>) -> BTreeSet<String> {
    let site = site_ctx.site;
    let mut keys = BTreeSet::new();
    for page in site_ctx.pages() {
        if page.draft {
            continue;
        }
        let segments: Vec<&str> = page
            .url_path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        for depth in 1..segments.len() {
            let prefix = segments[..depth].join("/");
            // The viewer's projection, not the whole site: a real page at `prefix` that
            // this recipient may not read should not suppress the generated listing that
            // is their only way into the pages below it.
            if site_ctx.view.page(prefix.as_str()).is_none()
                && build_site_index_under_prefix(
                    site_ctx.pages_map(),
                    prefix.as_str(),
                    site.root.as_path(),
                )
                .is_some()
            {
                keys.insert(prefix);
            }
        }
    }
    keys
}

fn export_site_root_index(
    output: &Path,
    renderer: &Renderer,
    site_ctx: &SiteExportContext<'_>,
    all_sites: &[SiteListEntry],
    config_summary: &ConfigSummary,
) -> Result<()> {
    let site = site_ctx.site;
    let site_index = remap_site_index(
        build_site_index_context(site_ctx.pages_map(), site.root.as_path()),
        site_ctx,
    );
    let root_url = join_url(site_ctx.mount.as_str(), "");
    let ctx = PageTemplateContext {
        site: site_context(site_ctx),
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
            source_escaped: None,
            md_url: None,
            source_filename: None,
        },
        nav_flat: remap_nav_flat(site_ctx.nav_flat(), site_ctx),
        breadcrumbs: vec![Crumb {
            title: site.title.clone(),
            url: root_url.clone(),
        }],
        prev: None,
        next: None,
        site_index: Some(site_index),
        all_sites: all_sites.to_vec(),
        config: config_summary.clone(),
        live_reload: false,
        // A static bundle has no server to mint against, so it never carries the control.
        share_control: String::new(),
    };
    let html = renderer.render_page(&ctx)?;
    let path = url_to_output_path(output, &root_url);
    write_html(&path, output, &html)?;
    Ok(())
}

fn export_folder_index(
    output: &Path,
    renderer: &Renderer,
    site_ctx: &SiteExportContext<'_>,
    folder_key: &str,
    all_sites: &[SiteListEntry],
    config_summary: &ConfigSummary,
) -> Result<()> {
    let site = site_ctx.site;
    let folder_site_index = remap_site_index(
        build_site_index_under_prefix(site_ctx.pages_map(), folder_key, site.root.as_path())
            .with_context(|| format!("missing folder index for {}", folder_key))?,
        site_ctx,
    );
    let folder_url = join_url(site_ctx.mount.as_str(), folder_key);
    let synthetic_title = folder_key
        .rsplit('/')
        .next()
        .map(humanize)
        .unwrap_or_else(|| site.title.clone());
    let crumbs = breadcrumbs_for_index_path(
        site.title.as_str(),
        site_ctx.mount.as_str(),
        site_ctx.pages_map(),
        folder_key,
        synthetic_title.as_str(),
    );
    let ctx = PageTemplateContext {
        site: site_context(site_ctx),
        page: PageContext {
            title: synthetic_title,
            description: Some(format!(
                "Auto-generated listing of pages under `{}`.",
                folder_key
            )),
            url: folder_url.clone(),
            url_path: folder_key.to_string(),
            layout: "doc".to_string(),
            draft: false,
            headings: vec![],
            frontmatter: serde_json::json!({}),
            html: String::new(),
            source_escaped: None,
            md_url: None,
            source_filename: None,
        },
        nav_flat: remap_nav_flat(site_ctx.nav_flat(), site_ctx),
        breadcrumbs: crumbs,
        prev: None,
        next: None,
        site_index: Some(folder_site_index),
        all_sites: all_sites.to_vec(),
        config: config_summary.clone(),
        live_reload: false,
        // A static bundle has no server to mint against, so it never carries the control.
        share_control: String::new(),
    };
    let html = renderer.render_page(&ctx)?;
    let path = url_to_output_path(output, &folder_url);
    write_html(&path, output, &html)?;
    Ok(())
}

fn export_page(
    output: &Path,
    renderer: &Renderer,
    site_ctx: &SiteExportContext<'_>,
    page: &Page,
    all_sites: &[SiteListEntry],
    config_summary: &ConfigSummary,
    site_index: Option<SiteIndexContext>,
) -> Result<()> {
    let site = site_ctx.site;
    let crumbs = breadcrumbs(
        site.title.as_str(),
        site_ctx.mount.as_str(),
        site_ctx.pages_map(),
        page,
    );
    let (prev_page, next_page) = prev_next(site_ctx.pages_map(), page);
    let prev = prev_page.map(|neighbor| NeighborContext {
        title: neighbor.title.clone(),
        url: site_ctx.page_url(neighbor),
    });
    let next = next_page.map(|neighbor| NeighborContext {
        title: neighbor.title.clone(),
        url: site_ctx.page_url(neighbor),
    });
    let ctx = PageTemplateContext {
        site: site_context(site_ctx),
        page: {
            // Same fields the server builds, so a bundle and a live page carry the same
            // source block — which is what `export_matches_server` exists to hold true.
            let page_url = site_ctx.page_url(page);
            PageContext {
                title: page.title.clone(),
                description: page.description.clone(),
                url_path: page.url_path.clone(),
                layout: page.layout.clone(),
                draft: page.draft,
                headings: page.headings.clone(),
                frontmatter: page.frontmatter.clone(),
                html: page.html.clone(),
                source_escaped: Some(escape_for_script_block(&page.source_text())),
                md_url: Some(format!(
                    "{}{page_url}",
                    crate::server::routes::MD_ROUTE_PREFIX
                )),
                source_filename: Some(page.filename.clone()),
                url: page_url,
            }
        },
        nav_flat: remap_nav_flat(site_ctx.nav_flat(), site_ctx),
        breadcrumbs: crumbs,
        prev,
        next,
        site_index: site_index.map(|index| remap_site_index(index, site_ctx)),
        all_sites: all_sites.to_vec(),
        config: config_summary.clone(),
        live_reload: false,
        // A static bundle has no server to mint against, so it never carries the control.
        share_control: String::new(),
    };
    let html = renderer.render_page(&ctx)?;
    let path = url_to_output_path(output, &site_ctx.page_url(page));
    write_html(&path, output, &html)?;
    Ok(())
}

fn copy_site_static_files(output: &Path, site_ctx: &SiteExportContext<'_>) -> Result<()> {
    let site = site_ctx.site;
    for rel in iter_site_static_files(&site.root)? {
        // The same reasoning as US-18: an exported bundle whose images are unfiltered
        // leaks the content of pages that were correctly omitted.
        if !site.allows_path(&rel, site_ctx.viewer()) {
            continue;
        }
        let source = site.root.join(&rel);
        let target = if site_ctx.flat() {
            output.join(&rel)
        } else {
            let mount_prefix = site.mount.trim_start_matches('/');
            if mount_prefix.is_empty() {
                output.join(&rel)
            } else {
                output.join(mount_prefix).join(&rel)
            }
        };
        let bytes = std::fs::read(&source)
            .with_context(|| format!("reading static file {}", source.display()))?;
        write_bytes(&target, &bytes)?;
    }
    Ok(())
}

fn site_context(site_ctx: &SiteExportContext<'_>) -> SiteContext {
    let site = site_ctx.site;
    SiteContext {
        title: site.title.clone(),
        mount: site_ctx.mount.clone(),
        root_url: site_ctx.mount.clone(),
        color: site.color.clone(),
    }
}

fn site_list_entries_for_export(
    sites: &[Arc<Site>],
    flat_single: bool,
    viewer: Option<&str>,
) -> Vec<SiteListEntry> {
    sites
        .iter()
        .filter_map(|site| {
            let mount = if flat_single {
                "/".to_string()
            } else {
                site.mount.clone()
            };
            // Counts come from the exported projection, so the bundle does not announce
            // how many pages the recipient did not receive.
            let page_count = site.view(viewer).pages().count();
            if viewer.is_some() && page_count == 0 {
                return None;
            }
            Some(SiteListEntry {
                title: site.title.clone(),
                mount: mount.clone(),
                url: mount,
                page_count,
                color: site.color.clone(),
            })
        })
        .collect()
}

fn remap_nav_flat(
    nav_flat: Arc<Vec<crate::content::SidebarNavRow>>,
    site_ctx: &SiteExportContext<'_>,
) -> Arc<Vec<crate::content::SidebarNavRow>> {
    if !site_ctx.flat() {
        return nav_flat;
    }
    Arc::new(
        nav_flat
            .iter()
            .map(|row| crate::content::SidebarNavRow {
                depth: row.depth,
                title: row.title.clone(),
                filename: row.filename.clone(),
                modified_at_ms: row.modified_at_ms,
                stable_key: row.stable_key.clone(),
                url: row.url.as_ref().map(|url| site_ctx.remap_url(url)),
                is_index: row.is_index,
            })
            .collect(),
    )
}

fn remap_site_index(index: SiteIndexContext, site_ctx: &SiteExportContext<'_>) -> SiteIndexContext {
    if !site_ctx.flat() {
        return index;
    }
    SiteIndexContext {
        rows: index
            .rows
            .into_iter()
            .map(|row| crate::content::site_index::SiteIndexRow {
                depth: row.depth,
                is_folder_heading: row.is_folder_heading,
                title: row.title,
                description: row.description,
                url: row.url.map(|url| site_ctx.remap_url(&url)),
                path_label: row.path_label,
            })
            .collect(),
        content_root_display: index.content_root_display,
        lead: index.lead,
        hint: index.hint,
    }
}

fn url_to_output_path(output_root: &Path, url: &str) -> PathBuf {
    let trimmed = url.trim_start_matches('/');
    if trimmed.is_empty() {
        output_root.join("index.html")
    } else {
        output_root.join(trimmed).join("index.html")
    }
}

fn write_html(path: &Path, output_root: &Path, html: &str) -> Result<()> {
    let rewritten = rewrite_asset_urls(html, path, output_root);
    write_bytes(path, rewritten.as_bytes())
}

fn rewrite_asset_urls(html: &str, html_path: &Path, output_root: &Path) -> String {
    let prefix = relative_asset_prefix(html_path, output_root);
    let mut out = html.replace("/__assets/", &prefix);
    out = out.replace(
        "/__mdshelf/syntax.css",
        &format!("{prefix}{SYNTAX_CSS_FILE}"),
    );
    out
}

fn relative_asset_prefix(html_path: &Path, output_root: &Path) -> String {
    let parent = html_path.parent().unwrap_or(output_root);
    let rel = parent.strip_prefix(output_root).unwrap_or(parent);
    let depth = rel.components().count();
    if depth == 0 {
        "assets/".to_string()
    } else {
        format!("{}assets/", "../".repeat(depth))
    }
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent of {}", path.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_asset_prefix_at_root() {
        let output = PathBuf::from("/dist");
        let html = output.join("index.html");
        assert_eq!(relative_asset_prefix(&html, &output), "assets/");
    }

    #[test]
    fn relative_asset_prefix_nested() {
        let output = PathBuf::from("/dist");
        let html = output.join("docs/guide/intro/index.html");
        assert_eq!(relative_asset_prefix(&html, &output), "../../../assets/");
    }

    #[test]
    fn rewrite_asset_urls_uses_relative_paths() {
        let output = PathBuf::from("/dist");
        let html_path = output.join("docs/index.html");
        let input = r#"<link href="/__assets/css/main.css"><link href="/__mdshelf/syntax.css">"#;
        let out = rewrite_asset_urls(input, &html_path, &output);
        assert!(out.contains(r#"href="../assets/css/main.css""#));
        assert!(out.contains(r#"href="../assets/syntax-highlight.css""#));
    }

    #[test]
    fn normalize_mount_adds_leading_slash() {
        assert_eq!(normalize_mount("docs"), "/docs");
        assert_eq!(normalize_mount("/docs"), "/docs");
        assert_eq!(normalize_mount("/"), "/");
    }

    #[test]
    fn remap_mount_url_strips_site_prefix() {
        assert_eq!(remap_mount_url("/docs/welcome", "/docs", "/"), "/welcome");
        assert_eq!(remap_mount_url("/docs", "/docs", "/"), "/");
    }
}
