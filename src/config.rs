use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::auth::is_valid_email;

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

    /// Authentication settings. Absent means the server is entirely unauthenticated
    /// and behaves exactly as it did before auth existed.
    #[serde(default)]
    pub auth: Option<AuthConfig>,

    /// Shareable-link settings. Absent means the defaults, which only take effect on a
    /// server started with `--auth google` (S16).
    #[serde(default)]
    pub links: Option<LinksConfig>,

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

/// `[auth]` section. Presence alone does not enable auth; `mdshelf serve --auth google`
/// (or `provider` set here plus `enabled`) turns it on. Credentials are never read from
/// this file — they come from the environment (D14).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// Identity provider. Only "google" is supported.
    #[serde(default = "default_auth_provider")]
    pub provider: String,

    /// Absolute ceiling on a session before full re-authentication (D26).
    #[serde(default = "default_session_max_age")]
    pub session_max_age: String,

    /// Address used for the `mailto:` request-access link on the deny page (D24).
    #[serde(default)]
    pub owner_email: Option<String>,

    /// How long access-log entries are retained before pruning (D27).
    #[serde(default = "default_audit_retention")]
    pub audit_retention: String,

    /// Path to the SQLite sidecar. Defaults to `mdshelf.db` beside the config file.
    #[serde(default)]
    pub database: Option<PathBuf>,

    /// Path to the AEAD key file. Defaults to `~/.config/mdshelf/secret.key` (D19).
    #[serde(default)]
    pub key_file: Option<PathBuf>,

    /// How long `bad-link` entries are kept (S15). Much shorter than `audit_retention`
    /// because these rows are written by unauthenticated strangers (R6).
    #[serde(default = "default_bad_link_retention")]
    pub bad_link_retention: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            provider: default_auth_provider(),
            session_max_age: default_session_max_age(),
            owner_email: None,
            audit_retention: default_audit_retention(),
            database: None,
            key_file: None,
            bad_link_retention: default_bad_link_retention(),
        }
    }
}

impl AuthConfig {
    /// Parsed `session_max_age`. Validated at load time, so this cannot fail later.
    pub fn session_max_age(&self) -> Duration {
        parse_duration(&self.session_max_age).unwrap_or(DEFAULT_SESSION_MAX_AGE)
    }

    /// Parsed `audit_retention`. Validated at load time.
    pub fn audit_retention(&self) -> Duration {
        parse_duration(&self.audit_retention).unwrap_or(DEFAULT_AUDIT_RETENTION)
    }

    /// Parsed `bad_link_retention`. Validated at load time.
    pub fn bad_link_retention(&self) -> Duration {
        parse_duration(&self.bad_link_retention).unwrap_or(DEFAULT_BAD_LINK_RETENTION)
    }
}

/// `[links]` section (S19).
///
/// Every field has a default, so `[links]` only has to be written out when something
/// needs changing. Note that `enabled = false` is an incident kill switch and not a
/// delete: the rows stay, so turning it back on restores exactly the links that existed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinksConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// URL prefix the share routes live under. Normalized like a site mount.
    #[serde(default = "default_links_prefix")]
    pub prefix: String,

    /// Lifetime used when neither `--for` nor `--until` is given.
    #[serde(default = "default_link_lifetime")]
    pub default_lifetime: String,

    /// The cap. A request for longer is refused rather than silently clamped, because
    /// a link that quietly outlives what somebody asked for is the wrong surprise.
    #[serde(default = "default_link_max_lifetime")]
    pub max_lifetime: String,

    /// How long a revoked or expired row is kept before the sweep deletes it.
    #[serde(default = "default_revoked_retention")]
    pub revoked_retention: String,
}

impl Default for LinksConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prefix: default_links_prefix(),
            default_lifetime: default_link_lifetime(),
            max_lifetime: default_link_max_lifetime(),
            revoked_retention: default_revoked_retention(),
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
fn default_auth_provider() -> String {
    "google".to_string()
}
fn default_session_max_age() -> String {
    "30d".to_string()
}
fn default_audit_retention() -> String {
    "90d".to_string()
}
fn default_bad_link_retention() -> String {
    "7d".to_string()
}
/// The default share-link prefix (S30).
pub fn default_links_prefix() -> String {
    "/s".to_string()
}
fn default_link_lifetime() -> String {
    "1d".to_string()
}
fn default_link_max_lifetime() -> String {
    "30d".to_string()
}
fn default_revoked_retention() -> String {
    "90d".to_string()
}

const DEFAULT_SESSION_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const DEFAULT_AUDIT_RETENTION: Duration = Duration::from_secs(90 * 24 * 60 * 60);
const DEFAULT_BAD_LINK_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Parse a duration written as `<integer><unit>`, where unit is one of `s`, `m`, `h`, `d`, `w`.
/// Deliberately strict: a bare number or an unknown unit is an error rather than a guess,
/// because these values govern how long a session stays valid.
pub fn parse_duration(raw: &str) -> Result<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("duration is empty; expected a value such as `30d`");
    }
    let split_at = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| anyhow!("duration `{}` has no unit; expected e.g. `30d`", raw))?;
    if split_at == 0 {
        bail!(
            "duration `{}` does not start with a number; expected e.g. `30d`",
            raw
        );
    }
    let (number, unit) = trimmed.split_at(split_at);
    let value: u64 = number
        .parse()
        .with_context(|| format!("duration `{}` has an invalid number", raw))?;
    let seconds_per_unit = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        "w" => 7 * 24 * 60 * 60,
        other => bail!(
            "duration `{}` has unknown unit `{}`; use one of s, m, h, d, w",
            raw,
            other
        ),
    };
    let seconds = value
        .checked_mul(seconds_per_unit)
        .ok_or_else(|| anyhow!("duration `{}` overflows", raw))?;
    Ok(Duration::from_secs(seconds))
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

        if let Some(auth) = self.auth.as_mut() {
            if auth.provider != "google" {
                bail!(
                    "[auth] provider `{}` is not supported; only `google` is available.",
                    auth.provider
                );
            }
            parse_duration(&auth.session_max_age).context("[auth] session_max_age is invalid")?;
            parse_duration(&auth.audit_retention).context("[auth] audit_retention is invalid")?;
            parse_duration(&auth.bad_link_retention)
                .context("[auth] bad_link_retention is invalid")?;
            if let Some(owner) = auth.owner_email.as_deref()
                && !is_valid_email(owner)
            {
                bail!(
                    "[auth] owner_email `{}` is not a valid email address",
                    owner
                );
            }
            if let Some(database) = auth.database.as_mut() {
                *database = expand_and_resolve(database, &self.source_dir)?;
            }
            if let Some(key_file) = auth.key_file.as_mut() {
                *key_file = expand_and_resolve(key_file, &self.source_dir)?;
            }
        }

        if let Some(links) = self.links.as_mut() {
            // Named individually so a broken value points at the key that broke it
            // rather than at the section (US-6).
            parse_duration(&links.default_lifetime)
                .context("[links] default_lifetime is invalid")?;
            parse_duration(&links.max_lifetime).context("[links] max_lifetime is invalid")?;
            parse_duration(&links.revoked_retention)
                .context("[links] revoked_retention is invalid")?;
            links.prefix = normalize_mount(&links.prefix).context("[links] prefix is invalid")?;
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

        self.validate_link_prefix()?;
        Ok(())
    }

    /// S30: a site may not mount where the share routes live.
    ///
    /// Only enforced once something has opted into the feature — an explicit `[links]`
    /// or an `[auth]` section. A pre-feature vault that happens to serve a site at `/s`
    /// keeps working exactly as it did (NFR-1), and only starts failing when its
    /// operator turns on the thing that would actually collide.
    pub fn validate_link_prefix(&self) -> Result<()> {
        if self.links.is_none() && self.auth.is_none() {
            return Ok(());
        }
        let prefix = self.links_prefix();
        for site in &self.sites {
            let mount = site.mount();
            let collides = mount == prefix
                || mount.starts_with(&format!("{prefix}/"))
                || prefix.starts_with(&format!("{mount}/"));
            if collides {
                bail!(
                    "site mount `{mount}` collides with the share-link prefix `{prefix}`. \
                     Move the share route with [links] prefix = \"/share\", or mount the \
                     site somewhere else."
                );
            }
        }
        Ok(())
    }

    /// The normalized share-link prefix, defaults included.
    pub fn links_prefix(&self) -> String {
        let raw = self
            .links
            .as_ref()
            .map(|links| links.prefix.clone())
            .unwrap_or_else(default_links_prefix);
        normalize_mount(&raw).unwrap_or_else(|_| default_links_prefix())
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Config {
    /// Build a validated config around an existing directory, for tests.
    pub fn for_test(source_dir: PathBuf, sites: Vec<SiteConfig>) -> Self {
        let mut config = Self {
            host: default_host(),
            port: default_port(),
            server: ServerConfig::default(),
            theme: ThemeConfig::default(),
            auth: None,
            links: None,
            sites,
            source_dir,
        };
        config
            .normalize_and_validate()
            .expect("test config should validate");
        config
    }
}

#[cfg(any(test, feature = "test-support"))]
impl SiteConfig {
    /// A site rooted at `path`, mounted at `/docs`.
    pub fn for_test(path: &Path) -> Self {
        Self::for_test_at(path, "/docs", "Docs")
    }

    /// A site rooted at `path` with an explicit mount and title.
    pub fn for_test_at(path: &Path, mount: &str, title: &str) -> Self {
        Self {
            path: path.to_path_buf(),
            mount: Some(mount.to_string()),
            title: Some(title.to_string()),
            theme: None,
            color: None,
        }
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

/// mdshelf's per-user directory, `~/.config/mdshelf`.
///
/// Everything mdshelf owns on behalf of a user lives here — the config it falls back
/// to, the credentials written by `auth setup`, the at-rest encryption key, and the
/// ACME cache. Deliberately *not* beside a project-local `mdshelf.toml`: that directory
/// is frequently a git repository, and secrets do not belong in one.
pub fn user_config_dir() -> Result<PathBuf> {
    let home = dirs_home().ok_or_else(|| {
        anyhow!("HOME is not set; pass an explicit path instead of relying on the default")
    })?;
    Ok(home.join(".config/mdshelf"))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a config file plus a content directory, and load it.
    fn load_config(toml_body: &str) -> Result<Config> {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("content")).expect("content dir");
        let config_path = dir.path().join("mdshelf.toml");
        let body = format!("{toml_body}\n\n[[sites]]\npath = \"content\"\nmount = \"/docs\"\n");
        std::fs::write(&config_path, body).expect("writing config");
        Config::load(Some(&config_path))
    }

    /// The full anyhow chain, since the useful detail is often in a source error.
    fn load_error(toml_body: &str) -> String {
        match load_config(toml_body) {
            Ok(_) => panic!("expected this config to be refused:\n{toml_body}"),
            Err(error) => format!("{error:#}"),
        }
    }

    /// Everything mdshelf owns per-user must sit in one directory, the same one the
    /// config already lives in. An extra `~/.mdshelf` alongside `~/.config/mdshelf`
    /// would be two places to look and two places to forget.
    #[test]
    fn per_user_paths_all_live_under_the_config_directory() {
        let base = user_config_dir().expect("HOME is set in tests");
        assert!(
            base.ends_with(".config/mdshelf"),
            "unexpected config directory: {}",
            base.display()
        );

        for path in [
            crate::auth::crypto::default_key_path().expect("key path"),
            crate::auth::credentials_file().expect("credentials path"),
        ] {
            assert!(
                path.starts_with(&base),
                "{} escaped the config directory {}",
                path.display(),
                base.display()
            );
        }
    }

    #[test]
    fn auth_section_is_optional() {
        let config = load_config("").expect("a config without [auth] must load");
        assert!(config.auth.is_none());
    }

    #[test]
    fn auth_section_parses_every_field() {
        let config = load_config(
            r#"
[auth]
provider = "google"
session_max_age = "7d"
owner_email = "owner@corp.com"
audit_retention = "30d"
"#,
        )
        .expect("a full [auth] section must load");

        let auth = config.auth.expect("[auth] present");
        assert_eq!(auth.provider, "google");
        assert_eq!(auth.session_max_age(), Duration::from_secs(7 * 86_400));
        assert_eq!(auth.audit_retention(), Duration::from_secs(30 * 86_400));
        assert_eq!(auth.owner_email.as_deref(), Some("owner@corp.com"));
    }

    #[test]
    fn auth_defaults_are_applied() {
        let config = load_config("[auth]\n").expect("an empty [auth] must load");
        let auth = config.auth.expect("[auth] present");
        assert_eq!(auth.session_max_age(), Duration::from_secs(30 * 86_400));
        assert_eq!(auth.audit_retention(), Duration::from_secs(90 * 86_400));
    }

    #[test]
    fn invalid_session_max_age_is_a_startup_error() {
        for bad in ["forever", "30", "30y", ""] {
            let err = load_error(&format!("[auth]\nsession_max_age = \"{bad}\"\n"));
            assert!(
                err.contains("session_max_age"),
                "the error should name the field; got: {err}"
            );
        }
    }

    #[test]
    fn invalid_audit_retention_is_a_startup_error() {
        let err = load_error("[auth]\naudit_retention = \"soon\"\n");
        assert!(err.contains("audit_retention"), "got: {err}");
    }

    #[test]
    fn unsupported_provider_is_refused() {
        let err = load_error("[auth]\nprovider = \"okta\"\n");
        assert!(err.contains("okta"), "got: {err}");
    }

    #[test]
    fn invalid_owner_email_is_refused() {
        // owner_email becomes a mailto: link on the deny page, so a typo would send
        // access requests into the void.
        let err = load_error("[auth]\nowner_email = \"owner@corp\"\n");
        assert!(err.contains("owner_email"), "got: {err}");
    }

    #[test]
    fn unknown_auth_keys_are_refused() {
        // deny_unknown_fields must still hold, so a misspelled security-relevant key
        // fails loudly instead of being silently ignored.
        let err = load_error("[auth]\nsession_max_ago = \"30d\"\n");
        assert!(err.contains("session_max_ago"), "got: {err}");
    }

    #[test]
    fn links_section_parses_every_field() {
        let config = load_config(
            r#"
[links]
enabled = false
prefix = "/share"
default_lifetime = "1h"
max_lifetime = "7d"
revoked_retention = "14d"
"#,
        )
        .expect("a full [links] section must load");
        let links = config.links.expect("[links] present");
        assert!(!links.enabled);
        assert_eq!(links.prefix, "/share");
        assert_eq!(links.default_lifetime, "1h");
        assert_eq!(links.max_lifetime, "7d");
        assert_eq!(links.revoked_retention, "14d");
    }

    #[test]
    fn links_defaults_are_applied() {
        let config = load_config("[links]\n").expect("an empty [links] must load");
        let links = config.links.expect("[links] present");
        assert!(links.enabled);
        assert_eq!(links.prefix, "/s");
        assert_eq!(links.default_lifetime, "1d");
        assert_eq!(links.max_lifetime, "30d");
        assert_eq!(links.revoked_retention, "90d");
    }

    /// US-6: an unparseable duration fails startup, naming the offending key.
    #[test]
    fn an_unparseable_link_duration_names_its_key() {
        for (key, value) in [
            ("default_lifetime", "soon"),
            ("max_lifetime", "30"),
            ("revoked_retention", "90y"),
        ] {
            let err = load_error(&format!("[links]\n{key} = \"{value}\"\n"));
            assert!(err.contains(key), "the error should name {key}; got: {err}");
        }
        let err = load_error("[auth]\nbad_link_retention = \"soon\"\n");
        assert!(err.contains("bad_link_retention"), "got: {err}");
    }

    #[test]
    fn unknown_link_keys_are_refused() {
        let err = load_error("[links]\nprefx = \"/s\"\n");
        assert!(err.contains("prefx"), "got: {err}");
    }

    /// US-6/S30. The site loses, not the share route: an operator whose site genuinely
    /// lives at `/s` is told to move the share route instead.
    #[test]
    fn a_prefix_colliding_with_a_site_mount_is_refused() {
        // The fixture always mounts a site at /docs.
        let err = load_error("[links]\nprefix = \"/docs\"\n");
        assert!(err.contains("collides"), "got: {err}");
        assert!(err.contains("/docs"), "got: {err}");

        // Nesting either way is still a collision: /s/{token} would swallow /s/manual.
        let err = load_error("[links]\nprefix = \"/docs/share\"\n");
        assert!(err.contains("collides"), "got: {err}");
    }

    /// NFR-1: a pre-feature vault serving a site at the default prefix keeps working
    /// until its operator turns on something that would actually collide.
    #[test]
    fn the_default_prefix_only_collides_once_something_opts_in() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("content")).expect("content dir");
        let config_path = dir.path().join("mdshelf.toml");
        let sites = "[[sites]]\npath = \"content\"\nmount = \"/s\"\n";

        std::fs::write(&config_path, sites).expect("writing config");
        Config::load(Some(&config_path)).expect("a site at /s must load without auth or links");

        std::fs::write(&config_path, format!("[auth]\n\n{sites}")).expect("writing config");
        let error = Config::load(Some(&config_path))
            .expect_err("with [auth] present the collision must be refused");
        assert!(format!("{error:#}").contains("collides"));
    }

    #[test]
    fn the_resolved_prefix_is_normalized() {
        let config = load_config("[links]\nprefix = \"share/\"\n").expect("loads");
        assert_eq!(config.links_prefix(), "/share");
        let config = load_config("").expect("loads");
        assert_eq!(config.links_prefix(), "/s");
    }

    #[test]
    fn parse_duration_accepts_every_unit() {
        assert_eq!(parse_duration("45s").unwrap(), Duration::from_secs(45));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1_800));
        assert_eq!(parse_duration("12h").unwrap(), Duration::from_secs(43_200));
        assert_eq!(
            parse_duration("30d").unwrap(),
            Duration::from_secs(2_592_000)
        );
        assert_eq!(
            parse_duration("2w").unwrap(),
            Duration::from_secs(1_209_600)
        );
        assert_eq!(
            parse_duration(" 7d ").unwrap(),
            Duration::from_secs(604_800)
        );
    }

    #[test]
    fn parse_duration_rejects_ambiguous_values() {
        // A bare number is refused rather than assumed to be seconds: the difference
        // between 30 seconds and 30 days of session validity is not a guess worth making.
        assert!(parse_duration("30").is_err());
        assert!(parse_duration("").is_err());
        assert!(parse_duration("d30").is_err());
        assert!(parse_duration("30y").is_err());
        assert!(parse_duration("-5d").is_err());
        assert!(parse_duration("18446744073709551615w").is_err());
    }
}
