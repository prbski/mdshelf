use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use minijinja::{Environment, Value};
use serde::Serialize;

use crate::theme::ThemeStack;

/// Minijinja-based template renderer. Loads layouts and partials from the layered
/// `ThemeStack` so per-site -> global -> embedded default overrides are honored.
pub struct Renderer {
    inner: Arc<Inner>,
}

struct Inner {
    env: Environment<'static>,
    theme: ThemeStack,
    template_names: BTreeSet<String>,
}

#[derive(Serialize)]
pub struct PageTemplateContext {
    pub site: SiteContext,
    pub page: PageContext,
    pub nav_flat: Arc<Vec<crate::content::SidebarNavRow>>,
    pub breadcrumbs: Vec<Crumb>,
    pub prev: Option<NeighborContext>,
    pub next: Option<NeighborContext>,
    pub site_index: Option<crate::content::SiteIndexContext>,
    pub all_sites: Vec<SiteListEntry>,
    pub config: ConfigSummary,
    pub live_reload: bool,
}

#[derive(Serialize)]
pub struct HomeTemplateContext {
    pub all_sites: Vec<SiteListEntry>,
    pub config: ConfigSummary,
    pub live_reload: bool,
}

#[derive(Serialize)]
pub struct ErrorTemplateContext {
    pub status: u16,
    pub title: String,
    pub message: String,
    pub all_sites: Vec<SiteListEntry>,
    pub config: ConfigSummary,
    pub live_reload: bool,
}

#[derive(Serialize, Clone)]
pub struct SiteContext {
    pub title: String,
    pub mount: String,
    pub root_url: String,
    pub color: String,
}

#[derive(Serialize)]
pub struct PageContext {
    pub title: String,
    pub description: Option<String>,
    pub url: String,
    pub url_path: String,
    pub layout: String,
    pub draft: bool,
    pub headings: Vec<crate::render::Heading>,
    pub frontmatter: serde_json::Value,
    pub html: String,
}

#[derive(Serialize, Clone)]
pub struct Crumb {
    pub title: String,
    pub url: String,
}

#[derive(Serialize, Clone)]
pub struct NeighborContext {
    pub title: String,
    pub url: String,
}

#[derive(Serialize, Clone)]
pub struct SiteListEntry {
    pub title: String,
    pub mount: String,
    pub url: String,
    pub page_count: usize,
    pub color: String,
}

#[derive(Serialize, Clone)]
pub struct ConfigSummary {
    pub host: String,
    pub port: u16,
    pub theme_name: Option<String>,
}

impl Renderer {
    pub fn new(theme: &ThemeStack) -> Result<Self> {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|name| {
            if name.ends_with(".html") || name.ends_with(".xml") {
                minijinja::AutoEscape::Html
            } else {
                minijinja::AutoEscape::None
            }
        });

        let mut names = BTreeSet::new();
        for entry in theme.template_files()? {
            env.add_template_owned(entry.logical_name.clone(), entry.source.clone())
                .with_context(|| format!("loading template {}", entry.logical_name))?;
            names.insert(entry.logical_name);
        }

        Ok(Self {
            inner: Arc::new(Inner {
                env,
                theme: theme.clone(),
                template_names: names,
            }),
        })
    }

    pub fn template_names(&self) -> &BTreeSet<String> {
        &self.inner.template_names
    }

    pub fn theme(&self) -> &ThemeStack {
        &self.inner.theme
    }

    pub fn render_page(&self, ctx: &PageTemplateContext) -> Result<String> {
        let layout = &ctx.page.layout;
        let template_name = format!("layouts/{}.html", layout);
        let resolved = if self.inner.template_names.contains(&template_name) {
            template_name
        } else {
            "layouts/doc.html".to_string()
        };
        self.render_named(&resolved, ctx)
    }

    pub fn render_home(&self, ctx: &HomeTemplateContext) -> Result<String> {
        let name = if self.inner.template_names.contains("layouts/home.html") {
            "layouts/home.html"
        } else {
            "layouts/index.html"
        };
        self.render_named(name, ctx)
    }

    pub fn render_error(&self, ctx: &ErrorTemplateContext) -> Result<String> {
        let name = if self.inner.template_names.contains("layouts/error.html") {
            "layouts/error.html"
        } else {
            "layouts/base.html"
        };
        self.render_named(name, ctx)
    }

    fn render_named<T: Serialize>(&self, name: &str, ctx: &T) -> Result<String> {
        let tmpl = self
            .inner
            .env
            .get_template(name)
            .with_context(|| format!("template {} not found", name))?;
        let value = Value::from_serialize(ctx);
        let out = tmpl
            .render(value)
            .with_context(|| format!("rendering template {}", name))?;
        Ok(out)
    }
}

impl Clone for Renderer {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// File entry exposed by ThemeStack to the renderer.
#[derive(Debug, Clone)]
pub struct TemplateEntry {
    pub logical_name: String,
    pub source: String,
    /// Real on-disk path if loaded from a directory; `None` if loaded from the
    /// embedded default theme.
    #[allow(dead_code)]
    pub on_disk: Option<PathBuf>,
}
