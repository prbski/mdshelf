pub mod page;
pub mod site_index;
pub mod source;
pub mod tree;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::{error, warn};

use crate::acl::AclIndex;
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
    pages: Arc<BTreeMap<String, Page>>,
    nav_root: NavNode,
    /// The unfiltered view, used when auth is off.
    #[serde(skip)]
    full_view: Arc<SiteView>,
    /// Access rules gathered from this site's frontmatter. Skipped during serialization
    /// so rules can never be rendered into a page.
    #[serde(skip)]
    acl: AclIndex,
    /// Per-viewer views, keyed by ACL signature (D12). Viewers with identical effective
    /// access share one entry. The cache lives and dies with the `Site`, so any content
    /// or rule change discards it wholesale when the universe is rebuilt.
    #[serde(skip)]
    views: RwLock<HashMap<String, Arc<SiteView>>>,
}

/// One viewer's projection of a site.
///
/// Every surface — navigation, breadcrumbs, prev/next, the site index, search, the
/// sitemap — is derived from `pages`. Filtering once, here, is what makes it impossible
/// for one surface to be gated while another quietly is not.
#[derive(Debug)]
pub struct SiteView {
    pub pages: Arc<BTreeMap<String, Page>>,
    pub nav_flat: Arc<Vec<SidebarNavRow>>,
}

impl SiteView {
    pub fn page(&self, url_path: &str) -> Option<&Page> {
        self.pages.get(url_path)
    }

    pub fn pages(&self) -> impl Iterator<Item = &Page> {
        self.pages.values()
    }

    pub fn nav_flat(&self) -> Arc<Vec<SidebarNavRow>> {
        Arc::clone(&self.nav_flat)
    }

    pub fn pages_map(&self) -> &BTreeMap<String, Page> {
        &self.pages
    }
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
        // Rules are collected per *file on disk*, not per rendered page.
        //
        // The pages map is a presentation structure: it is keyed by URL, so two files
        // that resolve to the same URL collapse into one, and a file that fails to load
        // disappears from it entirely. Neither of those may take a rule with it —
        // `hr.md` shadowing `hr/index.md` used to silently discard that folder's `deny`,
        // leaving its whole subtree governed by whatever a broader rule granted.
        let mut rules: Vec<(PathBuf, crate::acl::AclBlock)> = Vec::new();

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
                    // A file mdshelf cannot read may still be a rule file. Treating it
                    // as absent would let a broken `hr/index.md` open its whole subtree,
                    // so it is recorded as poisoned instead: unreadable means deny (D10).
                    rules.push((
                        rel.clone(),
                        crate::acl::AclBlock {
                            allow: Vec::new(),
                            deny: Vec::new(),
                            errors: vec![crate::acl::AclError {
                                key: "frontmatter".to_string(),
                                message: format!("this file could not be read: {load_error}"),
                                line: None,
                            }],
                        },
                    ));
                    continue;
                }
            };
            rules.push((rel.clone(), page.acl.clone()));

            if let Some(replaced_page) = pages.insert(page.url_path.clone(), page) {
                warn!(
                    url_path = %replaced_page.url_path,
                    kept = %absolute_path.display(),
                    dropped = %replaced_page.fs_path.display(),
                    "duplicate url_path for this site; keeping the later file in scan order \
                     (both files' access rules remain in force)"
                );
            }
        }

        let nav_root = NavNode::build(&title, &mount, &pages);
        let nav_flat = Arc::new(flatten_nav_sidebar_rows(&nav_root, &pages));
        let pages = Arc::new(pages);
        let full_view = Arc::new(SiteView {
            pages: Arc::clone(&pages),
            nav_flat,
        });

        let mut acl = AclIndex::new();
        for (rel_path, block) in rules {
            if block.is_poisoned() {
                for error in &block.errors {
                    // D10: loud, and with enough detail to fix it. The page stays
                    // loaded but now denies everyone.
                    error!(
                        path = %rel_path.display(),
                        line = error.line.unwrap_or(0),
                        key = %error.key,
                        "invalid access rule: {}; this file is now unreadable by everyone",
                        error.message
                    );
                }
            }
            acl.insert(&rel_path, block);
        }

        Ok(Self {
            mount,
            title,
            color,
            root,
            theme: cfg.theme.clone(),
            pages,
            nav_root,
            full_view,
            acl,
            views: RwLock::new(HashMap::new()),
        })
    }

    /// This site's access rules.
    pub fn acl(&self) -> &AclIndex {
        &self.acl
    }

    /// The projection of this site for a given viewer.
    ///
    /// `None` means authorization is not in force, and yields the unfiltered site so an
    /// unauthenticated server does no extra work at all (NFR-2).
    pub fn view(&self, viewer: Option<&str>) -> Arc<SiteView> {
        match viewer {
            None => Arc::clone(&self.full_view),
            Some(email) => self.view_for(email),
        }
    }

    fn view_for(&self, email: &str) -> Arc<SiteView> {
        let signature = self.acl.signature(email);

        if let Ok(cache) = self.views.read()
            && let Some(existing) = cache.get(&signature)
        {
            return Arc::clone(existing);
        }

        let view = Arc::new(self.build_view(email));
        if let Ok(mut cache) = self.views.write() {
            cache.insert(signature, Arc::clone(&view));
        }
        view
    }

    fn build_view(&self, email: &str) -> SiteView {
        let pages: BTreeMap<String, Page> = self
            .pages
            .iter()
            .filter(|(_, page)| self.acl.allows(&page.rel_path, email))
            .map(|(url_path, page)| (url_path.clone(), page.clone()))
            .collect();
        let nav_root = NavNode::build(&self.title, &self.mount, &pages);
        let nav_flat = Arc::new(flatten_nav_sidebar_rows(&nav_root, &pages));
        SiteView {
            pages: Arc::new(pages),
            nav_flat,
        }
    }

    /// Whether `email` may read the file at `rel_path` within this site.
    ///
    /// Used for attachments and raw files, which have no `Page` to filter (US-18).
    pub fn allows_path(&self, rel_path: &std::path::Path, viewer: Option<&str>) -> bool {
        match viewer {
            None => true,
            Some(email) => self.acl.allows(rel_path, email),
        }
    }

    pub fn pages(&self) -> impl Iterator<Item = &Page> {
        self.pages.values()
    }

    pub fn page(&self, url_path: &str) -> Option<&Page> {
        self.pages.get(url_path)
    }

    pub fn nav_flat(&self) -> Arc<Vec<SidebarNavRow>> {
        Arc::clone(&self.full_view.nav_flat)
    }

    pub fn pages_map(&self) -> &BTreeMap<String, Page> {
        &self.pages
    }
}
