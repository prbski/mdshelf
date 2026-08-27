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
    /// The Share control, already rendered (S25).
    ///
    /// Empty when there is nothing to offer — auth off, links disabled, or a surface
    /// that is not a page. A theme places it with `{{ share_control | safe }}`; one that
    /// never mentions it simply has no sharing, and renders without error (US-14).
    pub share_control: String,
}

#[derive(Serialize)]
pub struct HomeTemplateContext {
    pub all_sites: Vec<SiteListEntry>,
    pub config: ConfigSummary,
    pub live_reload: bool,
}

/// The reading view a share-link recipient gets (S6/S31).
///
/// Carries no site name, no logo, no navigation and no site switcher — only what is
/// needed to read one page and know where it came from. `site` holds a colour and
/// nothing else, so a theme that renders it cannot accidentally name the site.
#[derive(Serialize)]
pub struct LinkTemplateContext {
    pub page: LinkPageContext,
    pub banner: LinkBannerContext,
    pub site: LinkSiteContext,
    /// The live-reload client for this one link, already carrying the token (S27).
    ///
    /// Pre-rendered rather than assembled in the template: the socket path holds a
    /// token, and building a script literal out of template interpolation is exactly
    /// where an escaping mistake would become an injection.
    pub reload_script: String,
    pub live_reload: bool,
    pub config: ConfigSummary,
}

#[derive(Serialize)]
pub struct LinkPageContext {
    pub title: String,
    pub html: String,
}

#[derive(Serialize)]
pub struct LinkBannerContext {
    /// The address that issued the link. Reaches everyone the URL reaches (R1).
    pub issuer: String,
    /// Remaining time, already humanised: "20 hours".
    pub expires_in: String,
}

#[derive(Serialize, Clone)]
pub struct LinkSiteContext {
    pub color: String,
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
    /// This page's Markdown source, escaped for a `<script type="text/markdown">`
    /// block (§7.1). `None` for a surface with no `.md` file behind it — the home
    /// page, an error page, or an auto-generated folder index — which is what drives
    /// the disabled page-actions items.
    ///
    /// Escaped here rather than in the template because minijinja's autoescape is HTML
    /// entity escaping, and entities are not decoded inside a script element. The
    /// template therefore has to use `| safe`, and this field is what makes that sound.
    pub source_escaped: Option<String>,
    /// Absolute path of the download route for this page, e.g.
    /// `/__mdshelf/md/docs/guides/setup`. Emitted server-side so the browser never has
    /// to reason about which site mount it is under.
    pub md_url: Option<String>,
    /// The on-disk basename the download is saved as, e.g. `setup.md`.
    pub source_filename: Option<String>,
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
    pub version: String,
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

    /// Render the reading view.
    ///
    /// Falls back to a self-contained page when the theme has no `layouts/link.html`
    /// (R5): a custom theme that has never heard of share links must still be able to
    /// serve one, rather than answering a recipient with a template error.
    pub fn render_link(&self, ctx: &LinkTemplateContext) -> Result<String> {
        if self.inner.template_names.contains("layouts/link.html") {
            return self.render_named("layouts/link.html", ctx);
        }
        Ok(crate::links::pages::reading_view_fallback(ctx))
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
