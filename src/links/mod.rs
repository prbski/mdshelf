//! Shareable single-page links.
//!
//! A link is a bearer credential that reaches exactly one page plus the assets that
//! page references (S1/SEC-4). It is not a standalone grant: every request revalidates
//! the issuer's own access to that page, so a link dies when its issuer's access does
//! (S29/SEC-5).
//!
//! Nothing here implements `Debug` or `Display` for a token. That is deliberate, and
//! mirrors [`crate::auth::crypto`]: SEC-2 requires that no plaintext token can reach a
//! log line, an error message, or a database column, and the cheapest way to guarantee
//! it is to make the value unprintable.

pub mod commands;
pub mod control;
pub mod pages;
pub mod time;

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::config::{Config, LinksConfig, parse_duration};

/// SEC-1: 16 bytes from the OS CSPRNG. 128 bits is unguessable and still short enough
/// to paste comfortably.
pub const TOKEN_BYTES: usize = 16;

/// Length of the base64url, unpadded encoding of [`TOKEN_BYTES`].
pub const TOKEN_CHARS: usize = 22;

/// Bytes of the token hash that become the public link id (S14).
const ID_BYTES: usize = 3;

/// A freshly minted token, shown once and never recoverable (S5).
///
/// Deliberately has no `Debug`, no `Display`, no `Serialize` and no `Clone`: the only
/// way to read it is [`LinkToken::expose`], which every call site has to spell out.
pub struct LinkToken {
    value: String,
}

impl LinkToken {
    /// A new token from the OS CSPRNG.
    pub fn generate() -> Self {
        let mut buf = [0u8; TOKEN_BYTES];
        rand::fill(&mut buf[..]);
        Self {
            value: URL_SAFE_NO_PAD.encode(buf),
        }
    }

    /// The plaintext token. Only the one line that prints the URL may call this.
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// `sha256(token)`, the only form that is ever stored (SEC-1).
    pub fn hash(&self) -> [u8; 32] {
        token_hash(&self.value)
    }

    /// The public id, derived from the hash so it reveals nothing about the token.
    pub fn id(&self) -> String {
        link_id(&self.hash())
    }
}

/// `sha256(token)`.
pub fn token_hash(token: &str) -> [u8; 32] {
    let digest = Sha256::digest(token.as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// The short id used in listings, in `share revoke`, and as the access-log pseudonym
/// (S14). Derived from the token hash, so it discloses nothing about the token itself.
pub fn link_id(hash: &[u8]) -> String {
    let mut out = String::with_capacity(ID_BYTES * 2);
    for byte in hash.iter().take(ID_BYTES) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The pseudonym a link read is recorded under in the access log (S14).
pub fn link_pseudonym(id: &str) -> String {
    format!("link:{id}")
}

/// The pseudonym an unknown token is recorded under (S15).
///
/// Constant, because anything derived from the presented token would put a token
/// fingerprint in the database (SEC-2).
pub const BAD_LINK_PSEUDONYM: &str = "link:unknown";

/// Whether a path segment could be a token at all.
///
/// Used only to bound the work done for obvious junk; a well-formed token that is not
/// in the table is answered exactly like a malformed one (SEC-3), so this is never a
/// second class of rejection.
pub fn is_wellformed_token(candidate: &str) -> bool {
    candidate.len() == TOKEN_CHARS
        && candidate
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Resolved `[links]` settings.
#[derive(Debug, Clone)]
pub struct LinkSettings {
    /// The incident kill switch (S19). Rows survive; nothing serves.
    pub enabled: bool,
    /// Route prefix, normalized like a site mount. Defaults to `/s` (S30).
    pub prefix: String,
    pub default_lifetime: Duration,
    pub max_lifetime: Duration,
    pub revoked_retention: Duration,
}

impl Default for LinkSettings {
    fn default() -> Self {
        Self::from_links_config(&LinksConfig::default())
    }
}

impl LinkSettings {
    /// Resolve from a loaded config. Durations were validated at load time, so the
    /// fallbacks here can never be reached by a config mdshelf accepted.
    pub fn from_config(config: &Config) -> Self {
        match config.links.as_ref() {
            Some(links) => Self::from_links_config(links),
            None => Self::from_links_config(&LinksConfig::default()),
        }
    }

    fn from_links_config(links: &LinksConfig) -> Self {
        Self {
            enabled: links.enabled,
            prefix: crate::config::normalize_mount(&links.prefix)
                .unwrap_or_else(|_| crate::config::default_links_prefix()),
            default_lifetime: parse_duration(&links.default_lifetime)
                .unwrap_or(DEFAULT_LINK_LIFETIME),
            max_lifetime: parse_duration(&links.max_lifetime).unwrap_or(DEFAULT_MAX_LIFETIME),
            revoked_retention: parse_duration(&links.revoked_retention)
                .unwrap_or(DEFAULT_REVOKED_RETENTION),
        }
    }

    /// The URL of a link, given the origin browsers reach this server on.
    pub fn url(&self, base_url: &str, token: &LinkToken) -> String {
        format!(
            "{}{}/{}",
            base_url.trim_end_matches('/'),
            self.prefix,
            token.expose()
        )
    }

    /// Whether `path` is routed to the link handler.
    pub fn owns_path(&self, path: &str) -> bool {
        path == self.prefix || path.starts_with(&format!("{}/", self.prefix))
    }
}

/// The expiry instant a request asks for, or an error naming the rule it broke.
///
/// One implementation, shared by `mdshelf share` and the mint endpoint, so the cap the
/// browser enforces and the cap the server enforces cannot drift apart (US-15).
pub fn resolve_expiry(
    settings: &LinkSettings,
    for_duration: Option<&str>,
    until: Option<&str>,
    now: i64,
) -> anyhow::Result<i64> {
    use anyhow::{Context, bail};

    if for_duration.is_some() && until.is_some() {
        bail!("--for and --until say the same thing two different ways; pass one");
    }
    let expires_at = match (for_duration, until) {
        (Some(raw), _) => {
            let duration = crate::config::parse_duration(raw).context("--for is invalid")?;
            now + duration.as_millis() as i64
        }
        (None, Some(raw)) => time::parse_until(raw).context("--until is invalid")?,
        (None, None) => now + settings.default_lifetime.as_millis() as i64,
    };

    if expires_at <= now {
        bail!(
            "that expiry is in the past ({}); nothing was created",
            time::format_instant(expires_at)
        );
    }
    if expires_at - now > settings.max_lifetime.as_millis() as i64 {
        bail!(
            "that would outlive the {} cap in [links] max_lifetime; nothing was created",
            humanize_cap(settings.max_lifetime)
        );
    }
    Ok(expires_at)
}

fn humanize_cap(cap: Duration) -> String {
    let days = cap.as_secs() / 86_400;
    if days > 0 {
        format!("{days}d")
    } else {
        format!("{}s", cap.as_secs())
    }
}

/// The last day a `--until` date may name, as `YYYY-MM-DD`.
///
/// Handed to the browser as the date field's `max`, so the cap is visible before the
/// form is submitted as well as enforced after (US-15).
pub fn latest_allowed_date(settings: &LinkSettings, now: i64) -> String {
    let latest = now + settings.max_lifetime.as_millis() as i64;
    let (year, month, day) = time::civil_from_days(latest.div_euclid(86_400_000));
    format!("{year:04}-{month:02}-{day:02}")
}

/// The first day a `--until` date may name, as `YYYY-MM-DD`.
///
/// Today, in UTC: a bare date means the *end* of that day (`time::parse_until`), so
/// today is still a future expiry. Handed to the browser as the date field's `min`, so
/// the popover cannot offer a day [`resolve_expiry`] would reject as already past.
pub fn earliest_allowed_date(now: i64) -> String {
    let (year, month, day) = time::civil_from_days(now.div_euclid(86_400_000));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Record a new link, retrying if its short id happens to already exist.
///
/// The id is six hex characters, so a collision is rare but not impossible; a retry is
/// cheaper than either widening the id or failing in front of somebody.
pub fn mint(
    store: &crate::auth::store::Store,
    site: &str,
    path: &str,
    expires_at: i64,
    now: i64,
    issuer: &str,
) -> anyhow::Result<LinkToken> {
    for _ in 0..8 {
        let token = LinkToken::generate();
        let id = token.id();
        if store.link_by_id(&id)?.is_some() {
            continue;
        }
        match store.insert_link(&id, &token.hash(), site, path, expires_at, now, issuer) {
            Ok(()) => return Ok(token),
            // Never let a token or a hash reach an error (SEC-2).
            Err(error) if is_unique_violation(&error) => continue,
            Err(error) => {
                return Err(error.context("recording the new link"));
            }
        }
    }
    anyhow::bail!("could not allocate a unique link id after several attempts")
}

fn is_unique_violation(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<rusqlite::Error>()
        .and_then(|error| match error {
            rusqlite::Error::SqliteFailure(failure, _) => Some(failure.code),
            _ => None,
        })
        .is_some_and(|code| code == rusqlite::ErrorCode::ConstraintViolation)
}

pub const DEFAULT_LINK_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
pub const DEFAULT_MAX_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const DEFAULT_REVOKED_RETENTION: Duration = Duration::from_secs(90 * 24 * 60 * 60);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn tokens_are_unpadded_base64url_of_sixteen_bytes() {
        let token = LinkToken::generate();
        assert_eq!(token.expose().len(), TOKEN_CHARS);
        assert!(!token.expose().contains('='), "padding must be stripped");
        assert!(is_wellformed_token(token.expose()));
        assert_eq!(
            URL_SAFE_NO_PAD.decode(token.expose()).unwrap().len(),
            TOKEN_BYTES
        );
    }

    /// US-2: 10,000 generated tokens contain no duplicate.
    #[test]
    fn ten_thousand_tokens_contain_no_duplicate() {
        let mut seen = HashSet::with_capacity(10_000);
        for _ in 0..10_000 {
            assert!(
                seen.insert(LinkToken::generate().expose().to_string()),
                "the generator repeated a token"
            );
        }
        assert_eq!(seen.len(), 10_000);
    }

    #[test]
    fn the_id_is_derived_from_the_hash_and_not_the_token() {
        let token = LinkToken::generate();
        let id = token.id();
        assert_eq!(id.len(), ID_BYTES * 2);
        assert!(
            !token.expose().contains(&id) && !id.contains(token.expose()),
            "the id must not be a slice of the token"
        );
        assert_eq!(id, link_id(&token.hash()));
    }

    #[test]
    fn hashing_is_stable_and_differs_between_tokens() {
        assert_eq!(token_hash("abc"), token_hash("abc"));
        assert_ne!(token_hash("abc"), token_hash("abd"));
    }

    #[test]
    fn malformed_tokens_are_recognised_as_malformed() {
        assert!(!is_wellformed_token(""));
        assert!(!is_wellformed_token("short"));
        assert!(!is_wellformed_token(&"a".repeat(TOKEN_CHARS + 1)));
        assert!(!is_wellformed_token(&format!(
            "{}/",
            "a".repeat(TOKEN_CHARS - 1)
        )));
        assert!(is_wellformed_token(&"a".repeat(TOKEN_CHARS)));
    }

    #[test]
    fn the_prefix_owns_only_its_own_subtree() {
        let settings = LinkSettings::default();
        assert_eq!(settings.prefix, "/s");
        assert!(settings.owns_path("/s"));
        assert!(settings.owns_path("/s/abc"));
        assert!(!settings.owns_path("/system"));
        assert!(!settings.owns_path("/docs/s"));
    }

    #[test]
    fn the_url_has_no_path_component_beyond_the_token() {
        let settings = LinkSettings::default();
        let token = LinkToken::generate();
        let url = settings.url("https://work.example.com/", &token);
        assert_eq!(
            url,
            format!("https://work.example.com/s/{}", token.expose())
        );
        assert_eq!(url.matches('/').count(), 4, "got: {url}");
    }
}
