use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use include_dir::{Dir, include_dir};
use walkdir::WalkDir;

use crate::config::Config;
use crate::render::templates::TemplateEntry;

static EMBEDDED_THEME: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/theme");

/// Layered theme stack resolved from configuration. Each layer is a directory
/// (or the embedded default) and is consulted in priority order: per-site
/// overrides > global override > built-in default.
#[derive(Debug, Clone)]
pub struct ThemeStack {
    /// Optional global override directory (from `[theme].directory`).
    pub global_override: Option<PathBuf>,
    /// Per-site override directories indexed by site mount (from `[[sites]].theme`).
    pub per_site_overrides: BTreeMap<String, PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ResolvedAsset {
    pub bytes: Vec<u8>,
    #[allow(dead_code)]
    pub on_disk: Option<PathBuf>,
}

impl ThemeStack {
    pub fn from_config(config: &Config) -> Result<Self> {
        let global_override = config.theme.directory.clone();
        if let Some(dir) = &global_override
            && !dir.is_dir()
        {
            anyhow::bail!("global theme directory {} does not exist", dir.display());
        }

        let mut per_site_overrides = BTreeMap::new();
        for site in &config.sites {
            if let Some(dir) = &site.theme {
                if !dir.is_dir() {
                    anyhow::bail!(
                        "per-site theme directory {} for {} does not exist",
                        dir.display(),
                        site.mount()
                    );
                }
                per_site_overrides.insert(site.mount().to_string(), dir.clone());
            }
        }

        Ok(Self {
            global_override,
            per_site_overrides,
        })
    }

    /// Watchable directories so the live-reload watcher can observe theme changes.
    pub fn watch_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(d) = &self.global_override {
            dirs.push(d.clone());
        }
        dirs.extend(self.per_site_overrides.values().cloned());
        dirs
    }

    /// Build the merged set of templates that should be loaded into MiniJinja.
    /// For per-site overrides we expose them under the namespaced logical name
    /// `__site<mount>/<relative>` so layouts can reference them explicitly.
    /// The base templates (used by render_page/etc.) come from the global stack.
    pub fn template_files(&self) -> Result<Vec<TemplateEntry>> {
        let mut by_name: BTreeMap<String, TemplateEntry> = BTreeMap::new();

        for entry in iter_embedded_templates() {
            by_name.insert(entry.logical_name.clone(), entry);
        }

        if let Some(dir) = &self.global_override {
            for entry in iter_dir_templates(dir, None)? {
                by_name.insert(entry.logical_name.clone(), entry);
            }
        }

        for (mount, dir) in &self.per_site_overrides {
            let prefix = format!("__site{}/", mount.trim_start_matches('/'));
            for mut entry in iter_dir_templates(dir, Some(&prefix))? {
                let name = entry.logical_name.clone();
                entry.logical_name = name.clone();
                by_name.insert(name, entry);
            }
        }

        Ok(by_name.into_values().collect())
    }

    /// Resolve a theme asset at `relative` (e.g. `assets/css/main.css`) for the
    /// given site mount. Layered: per-site -> global -> embedded.
    pub fn resolve_asset(&self, mount: Option<&str>, relative: &str) -> Option<ResolvedAsset> {
        if let Some(m) = mount
            && let Some(dir) = self.per_site_overrides.get(m)
            && let Some(asset) = read_dir_asset(dir, relative)
        {
            return Some(asset);
        }
        if let Some(dir) = &self.global_override
            && let Some(asset) = read_dir_asset(dir, relative)
        {
            return Some(asset);
        }
        read_embedded_asset(relative)
    }
}

fn iter_embedded_templates() -> Vec<TemplateEntry> {
    let mut out = Vec::new();
    walk_embedded(&EMBEDDED_THEME, &mut |relative, file| {
        if !is_template_path(relative) {
            return;
        }
        if let Ok(text) = std::str::from_utf8(file.contents()) {
            out.push(TemplateEntry {
                logical_name: relative.to_string(),
                source: text.to_string(),
                on_disk: None,
            });
        }
    });
    out
}

fn iter_dir_templates(dir: &Path, name_prefix: Option<&str>) -> Result<Vec<TemplateEntry>> {
    let mut out = Vec::new();
    for entry in WalkDir::new(dir).follow_links(true) {
        let entry = entry.with_context(|| format!("walking {}", dir.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(dir)
            .map_err(|_| anyhow!("path escaped theme dir"))?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if !is_template_path(&rel_str) {
            continue;
        }
        let source = std::fs::read_to_string(entry.path())
            .with_context(|| format!("reading template {}", entry.path().display()))?;
        let logical_name = match name_prefix {
            Some(prefix) => format!("{}{}", prefix, rel_str),
            None => rel_str.clone(),
        };
        out.push(TemplateEntry {
            logical_name,
            source,
            on_disk: Some(entry.path().to_path_buf()),
        });
    }
    Ok(out)
}

fn is_template_path(rel: &str) -> bool {
    (rel.starts_with("layouts/") || rel.starts_with("partials/"))
        && (rel.ends_with(".html") || rel.ends_with(".xml") || rel.ends_with(".txt"))
}

fn read_dir_asset(dir: &Path, relative: &str) -> Option<ResolvedAsset> {
    let safe = sanitize_relative(relative)?;
    let path = dir.join(safe);
    let bytes = std::fs::read(&path).ok()?;
    Some(ResolvedAsset {
        bytes,
        on_disk: Some(path),
    })
}

fn read_embedded_asset(relative: &str) -> Option<ResolvedAsset> {
    let safe = sanitize_relative(relative)?;
    let file = EMBEDDED_THEME.get_file(safe.to_string_lossy().as_ref())?;
    Some(ResolvedAsset {
        bytes: file.contents().to_vec(),
        on_disk: None,
    })
}

fn sanitize_relative(relative: &str) -> Option<PathBuf> {
    let trimmed = relative.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let mut out = PathBuf::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return None;
        }
        out.push(segment);
    }
    Some(out)
}

fn walk_embedded<'a>(dir: &Dir<'a>, cb: &mut dyn FnMut(&str, &include_dir::File<'a>)) {
    for file in dir.files() {
        let relative = file.path().to_string_lossy().replace('\\', "/");
        cb(&relative, file);
    }
    for subdir in dir.dirs() {
        walk_embedded(subdir, cb);
    }
}

/// Extract the embedded default theme to a directory so users can fork and
/// customize it. Used by `mdshelf init --with-theme`.
pub fn extract_default_theme(dest: &Path, force: bool) -> Result<()> {
    if dest.exists() {
        if !force {
            anyhow::bail!(
                "{} already exists; pass --force to overwrite",
                dest.display()
            );
        }
        std::fs::remove_dir_all(dest).with_context(|| format!("removing {}", dest.display()))?;
    }
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    extract_dir(&EMBEDDED_THEME, dest)?;
    Ok(())
}

fn extract_dir(src: &Dir<'_>, dest: &Path) -> Result<()> {
    for file in src.files() {
        let rel = file.path();
        let target = dest.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&target, file.contents())
            .with_context(|| format!("writing {}", target.display()))?;
    }
    for subdir in src.dirs() {
        extract_dir(subdir, dest)?;
    }
    Ok(())
}
