use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub theme: ThemeConfig,

    #[serde(default)]
    pub sites: Vec<SiteConfig>,

    /// Absolute path of the loaded config file, used to resolve relative theme paths.
    #[serde(skip)]
    pub source_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_true")]
    pub live_reload: bool,

    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            live_reload: true,
            log_level: default_log_level(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    /// Name of a built-in theme. Currently only "mdshelf-theme" exists.
    pub name: Option<String>,

    /// Directory to use as a global theme override. Layered above the built-in default.
    pub directory: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteConfig {
    pub path: PathBuf,

    #[serde(default)]
    pub mount: Option<String>,

    #[serde(default)]
    pub title: Option<String>,

    /// Optional per-site theme directory. Layered above the global theme directory.
    #[serde(default)]
    pub theme: Option<PathBuf>,

    #[serde(default)]
    pub color: Option<String>,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    4444
}
fn default_true() -> bool {
    true
}
fn default_log_level() -> String {
    "info".to_string()
}

impl Config {
    /// Resolve the config file path (must exist) without reading it.
    pub fn resolve_path(path: Option<&Path>) -> Result<PathBuf> {
        let resolved = match path {
            Some(p) => p.to_path_buf(),
            None => Self::default_path()?,
        };
        resolved
            .canonicalize()
            .with_context(|| format!("resolving config path {}", resolved.display()))
    }

    /// Load and validate a config file. If `path` is `None`, looks at `./mdshelf.toml`
    /// then `~/.config/mdshelf/mdshelf.toml`.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let resolved = Self::resolve_path(path)?;

        let raw = std::fs::read_to_string(&resolved)
            .with_context(|| format!("reading config {}", resolved.display()))?;
        let mut config: Config = toml::from_str(&raw)
            .with_context(|| format!("parsing config {}", resolved.display()))?;

        config.source_dir = resolved
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        config.normalize_and_validate()?;
        Ok(config)
    }

    fn default_path() -> Result<PathBuf> {
        let local = PathBuf::from("mdshelf.toml");
        if local.exists() {
            return Ok(local);
        }
        if let Some(home) = dirs_home() {
            let candidate = home.join(".config/mdshelf/mdshelf.toml");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
        bail!(
            "no config file found. Pass --config PATH, or run `mdshelf init` (writes ~/.config/mdshelf/mdshelf.toml by default)."
        );
    }

    fn normalize_and_validate(&mut self) -> Result<()> {
        if self.sites.is_empty() {
            bail!("config defines no [[sites]]; nothing to serve.");
        }

        if let Some(dir) = self.theme.directory.as_mut() {
            *dir = expand_and_resolve(dir, &self.source_dir)?;
        }

        let palette = [
            "#10b981", // Emerald
            "#3b82f6", // Blue
            "#8b5cf6", // Violet
            "#f43f5e", // Rose
            "#f59e0b", // Amber
            "#0ea5e9", // Sky
            "#14b8a6", // Teal
            "#d946ef", // Fuchsia
            "#6366f1", // Indigo
        ];
        
        let mut seen = HashSet::new();
        for (i, site) in self.sites.iter_mut().enumerate() {
            site.path = expand_and_resolve(&site.path, &self.source_dir)?;
            if !site.path.is_dir() {
                bail!(
                    "site path {} is not a directory (or does not exist).",
                    site.path.display()
                );
            }
            if let Some(theme) = site.theme.as_mut() {
                *theme = expand_and_resolve(theme, &self.source_dir)?;
            }

            let mount = site
                .mount
                .clone()
                .unwrap_or_else(|| derive_mount(&site.path));
            let mount = normalize_mount(&mount)?;
            if !seen.insert(mount.clone()) {
                bail!("duplicate site mount: {}", mount);
            }
            site.mount = Some(mount);

            if site.title.is_none() {
                site.title = Some(derive_title(&site.path));
            }
            
            if site.color.is_none() {
                site.color = Some(palette[i % palette.len()].to_string());
            }
        }
        Ok(())
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

impl SiteConfig {
    pub fn mount(&self) -> &str {
        self.mount
            .as_deref()
            .expect("mount populated during validation")
    }
    pub fn title(&self) -> &str {
        self.title
            .as_deref()
            .expect("title populated during validation")
    }
    pub fn color(&self) -> &str {
        self.color
            .as_deref()
            .expect("color populated during validation")
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn expand_and_resolve(path: &Path, base: &Path) -> Result<PathBuf> {
    let as_str = path
        .to_str()
        .ok_or_else(|| anyhow!("path {} is not valid UTF-8", path.display()))?;
    let expanded = shellexpand::tilde(as_str).into_owned();
    let mut p = PathBuf::from(expanded);
    if p.is_relative() {
        p = base.join(p);
    }
    Ok(normalize_path(&p))
}

/// Resolve `..`/`.` segments without requiring the path to exist on disk.
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn derive_mount(path: &Path) -> String {
    path.file_name()
        .map(|s| format!("/{}", s.to_string_lossy()))
        .unwrap_or_else(|| "/".to_string())
}

fn derive_title(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Site".to_string())
}

/// Ensure the mount has exactly one leading slash, no trailing slash (except root),
/// and contains only URL-safe segments.
pub fn normalize_mount(mount: &str) -> Result<String> {
    let trimmed = mount.trim();
    if trimmed.is_empty() {
        bail!("site mount cannot be empty; use a prefix such as /docs");
    }
    let with_slash = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };
    let collapsed = with_slash.trim_end_matches('/').to_string();
    if collapsed.is_empty() {
        return Ok("/".to_string());
    }
    if collapsed == "/" {
        bail!(
            "mount '/' is not supported (it would conflict with the server home page at `/`). \
             Use a prefix such as `/docs` or `/wiki`."
        );
    }
    for segment in collapsed.trim_start_matches('/').split('/') {
        if segment.is_empty() {
            bail!("mount {} contains empty segment", mount);
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!(
                "mount {} segment '{}' has invalid characters",
                mount,
                segment
            );
        }
    }
    Ok(collapsed)
}
