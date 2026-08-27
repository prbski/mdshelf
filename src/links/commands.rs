//! `mdshelf share`, `share list` and `share revoke`.
//!
//! The CLI is both the scripting surface and the incident lever (S18). Unlike the
//! popover it has no authenticated identity, so the issuer it records is a label it was
//! handed rather than an address it verified — and because S29 revalidates that address
//! on every request, a link minted here is only useful if the label is a real reader.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};

use crate::auth::store::{LinkRecord, Store, now_ms};
use crate::cli::{ShareArgs, ShareListArgs, ShareRevokeArgs};
use crate::config::Config;
use crate::content::{Site, Universe};
use crate::links::LinkSettings;
use crate::links::time::format_instant;

/// The page a link will be minted for.
pub struct ShareTarget {
    pub site: Arc<Site>,
    /// Site-relative path of the source file, e.g. `hr/comp.md`.
    pub rel_path: PathBuf,
    /// The URL the page is served at, e.g. `/docs/hr/comp`.
    pub url: String,
}

/// `mdshelf share <path> [--for | --until]` (US-1).
pub fn create(args: &ShareArgs) -> Result<()> {
    let Some(raw_path) = args.path.as_deref() else {
        bail!("share needs a page: `mdshelf share /docs/hr/comp --for 1d`");
    };
    let config = Config::load(args.config.as_deref())?;
    require_auth_configured(&config)?;
    let settings = LinkSettings::from_config(&config);
    let base_url = resolve_base_url(&config, args.base_url.as_deref())?;

    let issuer = resolve_issuer(&config, args.as_issuer.as_deref())?;
    let universe = Universe::build(&config)?;
    let target = resolve_target(&universe, raw_path)?;

    let now = now_ms();
    let expires_at = crate::links::resolve_expiry(
        &settings,
        args.for_duration.as_deref(),
        args.until.as_deref(),
        now,
    )?;

    // S8: a link can never grant more than its issuer had. Refusing here turns a
    // guaranteed dead link into an error the operator can act on, rather than a URL
    // that silently answers with the deny page.
    if !target.site.allows_path(&target.rel_path, Some(&issuer)) {
        bail!(
            "{issuer} cannot read {} — a link would never serve, because a link is a \
             window onto its issuer's own access (S29). Grant access first with \
             `mdshelf acl grant {issuer} {}`.",
            target.url,
            target.rel_path.display()
        );
    }

    let store = open_or_create_store(&config)?;
    let token = crate::links::mint(
        &store,
        &site_key(&target.site),
        &crate::content::rel_path_key(&target.rel_path),
        expires_at,
        now,
        &issuer,
    )?;

    // The one place a plaintext token is allowed to exist outside the browser, and it
    // is never recoverable afterwards (S5).
    println!("{}", settings.url(&base_url, &token));
    Ok(())
}

/// `mdshelf share list` (US-3).
pub fn list(args: &ShareListArgs) -> Result<()> {
    let config = Config::load(args.config.as_deref())?;
    require_auth_configured(&config)?;
    let store = open_existing_store(&config)?;
    let universe = Universe::build(&config).ok();
    let now = now_ms();
    let links = store.list_links(now, args.all, None)?;

    if args.json {
        // S24: an inventory. It cannot restore access — no token is recoverable — but
        // it says exactly what was lost and what has to be minted again.
        let rows: Vec<serde_json::Value> = links
            .iter()
            .map(|link| {
                serde_json::json!({
                    "id": link.id,
                    "site": link.site,
                    "path": link.path,
                    "url": universe.as_ref().and_then(|u| page_url(u, link)),
                    "issued_by": link.issued_by,
                    "created_at": link.created_at,
                    "expires_at": link.expires_at,
                    "revoked_at": link.revoked_at,
                    "state": link.state(now),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if links.is_empty() {
        println!("no share links");
        return Ok(());
    }
    for link in &links {
        println!(
            "  {:<8} {:<8} {:<40} {:<28} {}",
            link.id,
            link.state(now),
            universe
                .as_ref()
                .and_then(|u| page_url(u, link))
                .unwrap_or_else(|| format!("{}:{}", link.site, link.path)),
            link.issued_by,
            format_instant(link.expires_at)
        );
    }
    println!();
    println!("{} link(s)", links.len());
    Ok(())
}

/// `mdshelf share revoke <id>` and `share revoke --all` (US-4).
pub fn revoke(args: &ShareRevokeArgs) -> Result<()> {
    let config = Config::load(args.config.as_deref())?;
    require_auth_configured(&config)?;
    let store = open_existing_store(&config)?;
    let now = now_ms();

    if args.all {
        let count = store.revoke_all_links(now)?;
        println!("revoked {count} live link(s)");
        return Ok(());
    }

    let Some(id) = args.id.as_deref() else {
        bail!("share revoke needs a link id, or --all");
    };
    if !store.revoke_link(id, now)? {
        bail!("no link with id `{id}`");
    }
    println!("revoked {id}");
    Ok(())
}

/// The issuer recorded for a CLI-minted link.
///
/// S20 calls this "a CLI-supplied label", but S29 resolves it against the ACL on every
/// request, so a label that is not a real reader produces a link that never serves.
/// `--as` is therefore the honest spelling, with `[auth] owner_email` as the default.
fn resolve_issuer(config: &Config, explicit: Option<&str>) -> Result<String> {
    let raw = explicit
        .map(str::to_string)
        .or_else(|| {
            config
                .auth
                .as_ref()
                .and_then(|auth| auth.owner_email.clone())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "a link is a window onto one person's access, so it needs an issuer.\n  \
                 Pass --as you@corp.com, or set [auth] owner_email."
            )
        })?;
    let email = crate::auth::normalize_email(&raw);
    if !crate::auth::is_valid_email(&email) {
        bail!("`{raw}` is not a valid email address");
    }
    Ok(email)
}

/// The origin the printed URL is built from (US-1).
pub fn resolve_base_url(config: &Config, explicit: Option<&str>) -> Result<String> {
    if let Some(raw) = explicit {
        let trimmed = raw.trim_end_matches('/');
        if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
            bail!("--base-url must start with http:// or https://; got `{raw}`");
        }
        return Ok(trimmed.to_string());
    }
    if crate::server::tls::is_loopback_host(&config.host) {
        return Ok(format!("http://{}:{}", config.host, config.port));
    }
    bail!(
        "mdshelf cannot tell what URL browsers use to reach this server: the config \
         binds {}, which is not an address anyone can visit.\n  Pass --base-url \
         https://your.domain",
        config.host
    )
}

/// Find the page a share names, however the operator spelled it (US-1).
pub fn resolve_target(universe: &Universe, raw: &str) -> Result<ShareTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("share needs a page path");
    }

    // A URL carrying a mount prefix, e.g. /docs/hr/comp.
    for site in universe.sites() {
        if let Some(tail) = trimmed.strip_prefix(site.mount.as_str())
            && (tail.is_empty() || tail.starts_with('/'))
            && let Some(target) = target_in_site(site, tail.trim_start_matches('/'))
        {
            return Ok(target);
        }
    }

    // Otherwise, a path relative to some site root.
    let relative = trimmed.trim_start_matches('/');
    for site in universe.sites() {
        if let Some(target) = target_in_site(site, relative) {
            return Ok(target);
        }
    }

    // A path that resolves nowhere. When something differs only in case, say so — that
    // is the failure an operator is most likely to be staring at without seeing, and on
    // a case-insensitive filesystem it is the one that looks like a bug in mdshelf.
    for candidate in spellings(universe, trimmed) {
        if let Some(suggestion) = closest_match(universe, &candidate) {
            bail!("no page at `{raw}`. Did you mean `{suggestion}`?");
        }
    }
    if !mount_owns(universe, trimmed) {
        bail!("no configured site serves `{raw}`");
    }
    bail!("no page at `{raw}` in any configured site")
}

/// Every site-relative spelling `raw` could stand for: the path as given, and the tail
/// under each site mount that prefixes it.
fn spellings(universe: &Universe, raw: &str) -> Vec<String> {
    let mut out = vec![raw.trim_start_matches('/').to_string()];
    for site in universe.sites() {
        if let Some(tail) = raw.strip_prefix(site.mount.as_str())
            && (tail.is_empty() || tail.starts_with('/'))
        {
            out.push(tail.trim_start_matches('/').to_string());
        }
    }
    out
}

fn target_in_site(site: &Arc<Site>, candidate: &str) -> Option<ShareTarget> {
    let page = crate::content::page::page_lookup_keys(candidate)
        .iter()
        .find_map(|key| site.page(key))?;
    // A draft is not published, so it is not shareable either.
    if page.draft {
        return None;
    }
    Some(ShareTarget {
        site: Arc::clone(site),
        rel_path: page.rel_path.clone(),
        url: page.url.clone(),
    })
}

fn mount_owns(universe: &Universe, path: &str) -> bool {
    universe
        .sites()
        .iter()
        .any(|site| path == site.mount.as_str() || path.starts_with(&format!("{}/", site.mount)))
}

/// A page whose spelling differs from `candidate` only in case.
fn closest_match(universe: &Universe, candidate: &str) -> Option<String> {
    let wanted = candidate.trim_matches('/');
    if wanted.is_empty() {
        return None;
    }
    for site in universe.sites() {
        for page in site.pages() {
            let rel = page.rel_path.to_string_lossy().replace('\\', "/");
            if page.url_path.eq_ignore_ascii_case(wanted) || rel.eq_ignore_ascii_case(wanted) {
                return Some(page.url.clone());
            }
        }
    }
    None
}

/// The stable key a link stores for its site: the canonicalised root path (S20).
///
/// Deliberately not the mount, which is a presentation choice an operator may rename
/// without meaning to break every link they have handed out.
pub fn site_key(site: &Site) -> String {
    site.root.to_string_lossy().replace('\\', "/")
}

/// The site a stored link belongs to, if it is still configured.
pub fn site_for_record<'a>(universe: &'a Universe, record: &LinkRecord) -> Option<&'a Arc<Site>> {
    universe
        .sites()
        .iter()
        .find(|site| site_key(site) == record.site)
}

/// The URL of a stored link's page, when its site is still configured.
pub fn page_url(universe: &Universe, record: &LinkRecord) -> Option<String> {
    let site = site_for_record(universe, record)?;
    let page = site
        .pages()
        .find(|page| page.rel_path.to_string_lossy().replace('\\', "/") == record.path)?;
    Some(page.url.clone())
}

fn require_auth_configured(config: &Config) -> Result<()> {
    if config.auth.is_none() {
        bail!(
            "share links require Google sign-in (S16): without it every page is already \
             public, so a link would imply protection that is not there.\n  Add an \
             [auth] section to your config and run the server with `--auth google`."
        );
    }
    Ok(())
}

fn database_path(config: &Config) -> PathBuf {
    config
        .auth
        .as_ref()
        .and_then(|auth| auth.database.clone())
        .unwrap_or_else(|| config.source_dir.join("mdshelf.db"))
}

fn open_or_create_store(config: &Config) -> Result<Store> {
    Store::open(&database_path(config))
}

fn open_existing_store(config: &Config) -> Result<Store> {
    let path = database_path(config);
    if !path.exists() {
        bail!(
            "no link database at {} yet — it is created by `mdshelf share` or the first \
             run of the server with `--auth google`.",
            path.display()
        );
    }
    Store::open(&path)
}
