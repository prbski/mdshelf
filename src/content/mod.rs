pub mod page;
pub mod site_index;
pub mod source;
pub mod tree;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::warn;

use crate::config::{Config, SiteConfig};
use crate::render::markdown::MarkdownRenderer;

pub use page::Page;
pub use site_index::{SiteIndexContext, build_site_index_context, build_site_index_under_prefix};
use tree::flatten_nav_sidebar_rows;
pub use tree::{NavNode, SidebarNavRow};

/// All sites loaded from the configuration.
#[derive(Debug, Serialize)]
pub struct Universe {
    pub sites: Vec<Arc<Site>>,
}

#[derive(Debug, Serialize)]
pub struct Site {
    pub mount: String,
    pub title: String,
    pub color: String,
    pub root: PathBuf,
    pub theme: Option<PathBuf>,
    pages: BTreeMap<String, Page>,
    nav_root: NavNode,
    nav_flat: Arc<Vec<SidebarNavRow>>,
}

impl Universe {
    pub fn build(config: &Config) -> Result<Self> {
        let renderer = MarkdownRenderer::new();
        let mut sites = Vec::with_capacity(config.sites.len());
        for site_cfg in &config.sites {
            sites.push(Arc::new(Site::build(site_cfg, &renderer)?));
        }
        Ok(Self { sites })
    }

    pub fn sites(&self) -> &[Arc<Site>] {
        &self.sites
    }
}

impl Site {
    fn build(cfg: &SiteConfig, renderer: &MarkdownRenderer) -> Result<Self> {
        let mount = cfg.mount().to_string();
        let title = cfg.title().to_string();
        let color = cfg.color().to_string();
        let root = cfg.path.clone();

        let rels = source::iter_markdown_files(&root)
            .with_context(|| format!("scanning site root {}", root.display()))?;

        let mut pages = BTreeMap::new();
        for rel in rels {
            if source::relative_path_has_hidden_component(rel.as_path()) {
                continue;
            }
            let absolute_path = root.join(&rel);
            let page = match Page::load(&root, &rel, &mount, renderer) {
                Ok(loaded_page) => loaded_page,
                Err(load_error) => {
                    warn!(
                        path = %absolute_path.display(),
                        error = %load_error,
                        "skipping markdown file (read error or invalid front matter)"
                    );
                    continue;
                }
            };
            if let Some(replaced_page) = pages.insert(page.url_path.clone(), page) {
                warn!(
                    url_path = %replaced_page.url_path,
                    kept = %absolute_path.display(),
                    dropped = %replaced_page.fs_path.display(),
                    "duplicate url_path for this site; keeping the later file in scan order"
                );
            }
        }

        let nav_root = NavNode::build(&title, &mount, &pages);
        let nav_flat = Arc::new(flatten_nav_sidebar_rows(&nav_root, &pages));

        Ok(Self {
            mount,
            title,
            color,
            root,
            theme: cfg.theme.clone(),
            pages,
            nav_root,
            nav_flat,
        })
    }

    pub fn pages(&self) -> impl Iterator<Item = &Page> {
        self.pages.values()
    }

    pub fn page(&self, url_path: &str) -> Option<&Page> {
        self.pages.get(url_path)
    }

    pub fn nav_flat(&self) -> Arc<Vec<SidebarNavRow>> {
        Arc::clone(&self.nav_flat)
    }

    pub fn pages_map(&self) -> &BTreeMap<String, Page> {
        &self.pages
    }
}
