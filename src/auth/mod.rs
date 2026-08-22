//! Google sign-in and session management.
//!
//! Authorization (who may read which path) lives in [`crate::acl`]; this module is only
//! concerned with establishing *who the visitor is*.

pub mod crypto;
pub mod email;
pub mod oidc;
pub mod pages;
pub mod routes;
pub mod setup;
pub mod store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tracing::warn;

use crate::config::{AuthConfig, Config};
use crypto::SecretKey;
use oidc::{Provider, RefreshFailure};
use store::{Store, now_ms};

pub use email::{is_valid_email, normalize_email};

pub const DOCS_URL: &str = "https://mdshelf.dev/docs/google-auth";
pub const CLIENT_ID_ENV: &str = "MDSHELF_GOOGLE_CLIENT_ID";
pub const CLIENT_SECRET_ENV: &str = "MDSHELF_GOOGLE_CLIENT_SECRET";
/// Testing override so the suite can point mdshelf at a local mock issuer.
pub const DISCOVERY_URL_ENV: &str = "MDSHELF_OIDC_DISCOVERY_URL";

pub const SESSION_COOKIE: &str = "mdshelf_session";

/// A session idle for longer than this is re-validated against the provider before it
/// is served again (D21). Chosen so an outage cannot interrupt somebody mid-read.
pub const IDLE_REFRESH_AFTER: Duration = Duration::from_secs(30 * 60);

/// An in-flight authorization request expires after this long (US-3).
pub const FLOW_TTL: Duration = Duration::from_secs(10 * 60);

/// Google OAuth client credentials.
///
/// Deliberately not `Debug`: the secret must be unable to reach a log line (NFR-4).
#[derive(Clone)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
}

impl Credentials {
    /// Read credentials from the environment, falling back to the file written by
    /// `mdshelf auth setup` (D14/D15).
    ///
    /// The environment wins, so a secret manager or container platform can override the
    /// on-disk copy without the operator having to delete it first.
    pub fn from_env() -> Result<Self> {
        let stored = credentials_file()
            .ok()
            .and_then(|path| read_env_file(&path));

        let client_id = env_or(CLIENT_ID_ENV, stored.as_ref())?;
        let client_secret = env_or(CLIENT_SECRET_ENV, stored.as_ref())?;
        Ok(Self {
            client_id,
            client_secret,
        })
    }
}

/// Where `mdshelf auth setup` stores credentials: `~/.config/mdshelf/credentials.env`.
pub fn credentials_file() -> Result<PathBuf> {
    Ok(crate::config::user_config_dir()
        .context("resolving where to store credentials")?
        .join("credentials.env"))
}

/// Parse a `KEY=value` file, ignoring blanks and comments.
fn read_env_file(path: &std::path::Path) -> Option<HashMap<String, String>> {
    let contents = std::fs::read_to_string(path).ok()?;
    let mut values = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }
    Some(values)
}

fn env_or(name: &str, stored: Option<&HashMap<String, String>>) -> Result<String> {
    if let Ok(value) = std::env::var(name)
        && !value.trim().is_empty()
    {
        return Ok(value);
    }
    if let Some(value) = stored.and_then(|values| values.get(name))
        && !value.trim().is_empty()
    {
        return Ok(value.clone());
    }
    bail!(
        "{name} is not set.\n  \
         mdshelf uses your own Google OAuth client, so no credentials ship with the binary.\n  \
         Run `mdshelf auth setup` for a guided walkthrough, or see {DOCS_URL}"
    )
}

/// Resolved, validated auth settings.
#[derive(Debug, Clone)]
pub struct AuthSettings {
    pub session_max_age: Duration,
    pub audit_retention: Duration,
    pub owner_email: Option<String>,
    /// Externally visible origin, e.g. `https://docs.acme.com`. Redirect URIs are built
    /// from this, so it must match what is registered with Google exactly.
    pub public_url: String,
    pub database_path: PathBuf,
    pub key_file_path: PathBuf,
}

impl AuthSettings {
    pub fn resolve(config: &Config, auth: &AuthConfig, public_url: String) -> Result<Self> {
        let database_path = match auth.database.clone() {
            Some(path) => path,
            None => config.source_dir.join("mdshelf.db"),
        };
        let key_file_path = match auth.key_file.clone() {
            Some(path) => path,
            None => crypto::default_key_path()?,
        };
        Ok(Self {
            session_max_age: auth.session_max_age(),
            audit_retention: auth.audit_retention(),
            owner_email: auth.owner_email.clone(),
            public_url: public_url.trim_end_matches('/').to_string(),
            database_path,
            key_file_path,
        })
    }

    /// The redirect URI registered with Google.
    ///
    /// Trims here as well as in [`AuthSettings::resolve`]: Google compares this string
    /// exactly, so a stray `//` is not a cosmetic problem — it makes every sign-in fail
    /// with `redirect_uri_mismatch`.
    pub fn redirect_uri(&self) -> String {
        format!("{}/auth/callback", self.public_url.trim_end_matches('/'))
    }

    /// Whether cookies may carry the `Secure` attribute (SEC-5).
    pub fn is_secure_origin(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}

/// An authorization request awaiting its callback.
struct PendingFlow {
    verifier: String,
    nonce: String,
    next: String,
    created_at: i64,
}

/// Everything auth needs at request time.
pub struct AuthRuntime {
    pub settings: AuthSettings,
    pub credentials: Credentials,
    pub provider: Provider,
    pub store: Store,
    key: SecretKey,
    flows: Mutex<HashMap<String, PendingFlow>>,
}

impl AuthRuntime {
    /// Build the runtime: read credentials, open the sidecar, load the key, discover
    /// the provider. Any failure here is a startup failure (US-1).
    pub async fn initialize(
        config: &Config,
        auth: &AuthConfig,
        public_url: String,
    ) -> Result<Self> {
        let credentials = Credentials::from_env()?;
        let discovery_url = std::env::var(DISCOVERY_URL_ENV)
            .unwrap_or_else(|_| oidc::GOOGLE_DISCOVERY_URL.to_string());
        Self::build(config, auth, public_url, credentials, &discovery_url).await
    }

    /// Build the runtime from explicit credentials and a specific issuer.
    ///
    /// Kept separate from [`AuthRuntime::initialize`] so tests can point at a local
    /// issuer without mutating process-global environment variables, which would make
    /// the suite racy under parallel execution.
    pub async fn build(
        config: &Config,
        auth: &AuthConfig,
        public_url: String,
        credentials: Credentials,
        discovery_url: &str,
    ) -> Result<Self> {
        let settings = AuthSettings::resolve(config, auth, public_url)?;
        let key = SecretKey::load_or_create(&settings.key_file_path)?;
        let store = Store::open(&settings.database_path)?;

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("building the HTTP client")?;
        let provider = Provider::discover(http, discovery_url).await?;

        Ok(Self {
            settings,
            credentials,
            provider,
            store,
            key,
            flows: Mutex::new(HashMap::new()),
        })
    }

    /// Register a pending authorization request, returning its `state`.
    pub fn begin_flow(&self, verifier: String, nonce: String, next: String) -> String {
        let state = crypto::random_token(32);
        let mut flows = self.lock_flows();
        prune_expired_flows(&mut flows);
        flows.insert(
            state.clone(),
            PendingFlow {
                verifier,
                nonce,
                next,
                created_at: now_ms(),
            },
        );
        state
    }

    /// Consume a pending flow. Returns `None` if the state is unknown, already used, or
    /// expired — all of which are treated identically by the callback (SEC-3).
    pub fn take_flow(&self, state: &str) -> Option<(String, String, String)> {
        let mut flows = self.lock_flows();
        prune_expired_flows(&mut flows);
        let flow = flows.remove(state)?;
        Some((flow.verifier, flow.nonce, flow.next))
    }

    fn lock_flows(&self) -> std::sync::MutexGuard<'_, HashMap<String, PendingFlow>> {
        self.flows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Create a session for a verified identity, storing the refresh token encrypted.
    pub fn create_session(&self, email: &str, refresh_token: Option<&str>) -> Result<String> {
        let id = crypto::random_token(32);
        let encrypted = match refresh_token {
            Some(token) => Some(self.key.encrypt(token.as_bytes())?),
            None => None,
        };
        self.store
            .insert_session(&id, email, now_ms(), encrypted.as_deref())?;
        Ok(id)
    }

    pub fn end_session(&self, session_id: &str) -> Result<()> {
        self.store.delete_session(session_id)
    }

    /// Resolve a cookie value into an identity, revalidating if the session went idle.
    ///
    /// Every path that does not return [`SessionOutcome::Active`] has already removed
    /// the session row, so a rejected cookie can never be replayed (US-7).
    pub async fn resolve_session(&self, session_id: &str) -> SessionOutcome {
        let record = match self.store.get_session(session_id) {
            Ok(Some(record)) => record,
            Ok(None) => return SessionOutcome::Anonymous,
            Err(error) => {
                warn!(%error, "session lookup failed; treating the request as anonymous");
                return SessionOutcome::Anonymous;
            }
        };

        let now = now_ms();

        // D26: the absolute ceiling, regardless of activity.
        if now.saturating_sub(record.created_at) > self.settings.session_max_age.as_millis() as i64
        {
            self.discard_session(&record.id, "session reached its maximum age");
            return SessionOutcome::Anonymous;
        }

        // D21: only a session that has gone idle is re-validated.
        let idle_ms = now.saturating_sub(record.last_seen_at);
        if idle_ms > IDLE_REFRESH_AFTER.as_millis() as i64
            && let Err(reason) = self.revalidate(&record).await
        {
            // D20: both an explicit rejection and an unreachable provider end the
            // session. The log line names which, because during an outage that
            // distinction is the entire diagnosis (R1).
            warn!(
                session = %redact_session_id(&record.id),
                email = %record.email,
                kind = reason.kind(),
                reason = reason.reason(),
                "session invalidated: provider re-validation failed"
            );
            self.discard_session(&record.id, "re-validation failed");
            return SessionOutcome::Anonymous;
        }

        if let Err(error) = self.store.touch_session(&record.id, now) {
            warn!(%error, "failed to update session last_seen_at");
        }
        SessionOutcome::Active(record.email)
    }

    async fn revalidate(
        &self,
        record: &store::SessionRecord,
    ) -> std::result::Result<(), RefreshFailure> {
        let Some(encrypted) = record.refresh_token_enc.as_deref() else {
            // No refresh token means the account can never be re-checked against the
            // provider, so the session cannot honour D18. Treat it as rejected.
            return Err(RefreshFailure::Rejected(
                "session has no stored refresh token".to_string(),
            ));
        };
        let plaintext = self.key.decrypt(encrypted).map_err(|_| {
            // A row that will not decrypt is unusable; the key changed or the row is
            // corrupt. Invalidate rather than panicking (US-6).
            RefreshFailure::Rejected("stored refresh token could not be decrypted".to_string())
        })?;
        let refresh_token = String::from_utf8(plaintext).map_err(|_| {
            RefreshFailure::Rejected("stored refresh token is not UTF-8".to_string())
        })?;

        self.provider
            .refresh(
                &self.credentials.client_id,
                &self.credentials.client_secret,
                &refresh_token,
            )
            .await
            .map(|_| ())
    }

    fn discard_session(&self, session_id: &str, reason: &str) {
        if let Err(error) = self.store.delete_session(session_id) {
            warn!(%error, reason, "failed to delete an invalidated session");
        }
    }

    /// Prune access-log entries past their retention window (D27).
    pub fn prune_audit(&self) {
        match self
            .store
            .prune_access_log(now_ms(), self.settings.audit_retention)
        {
            Ok(0) => {}
            Ok(removed) => tracing::debug!(removed, "pruned access log entries"),
            Err(error) => warn!(%error, "pruning the access log failed"),
        }
    }
}

/// The result of resolving a session cookie.
#[derive(Debug, Clone)]
pub enum SessionOutcome {
    /// No usable session. The visitor is treated as anonymous and sees the interstitial.
    Anonymous,
    /// A live session belonging to this verified address.
    Active(String),
}

fn prune_expired_flows(flows: &mut HashMap<String, PendingFlow>) {
    let cutoff = now_ms() - FLOW_TTL.as_millis() as i64;
    flows.retain(|_, flow| flow.created_at >= cutoff);
}

/// Session ids are bearer credentials; only a short prefix ever reaches a log line.
fn redact_session_id(id: &str) -> String {
    let visible: String = id.chars().take(6).collect();
    format!("{visible}…")
}

/// Validate a `next` parameter as a same-site path (SEC-4).
///
/// Anything that could send the browser to another origin after sign-in is rejected:
/// absolute URLs, protocol-relative `//host` paths, and backslash variants that some
/// browsers normalise into slashes.
pub fn sanitize_next(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return Some("/".to_string());
    }
    if !raw.starts_with('/') {
        return None;
    }
    if raw.starts_with("//") || raw.starts_with("/\\") {
        return None;
    }
    if raw.contains('\\') || raw.contains('\n') || raw.contains('\r') {
        return None;
    }
    if raw.contains("://") {
        return None;
    }
    Some(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_next_accepts_local_paths() {
        assert_eq!(sanitize_next("/hr/comp.md").as_deref(), Some("/hr/comp.md"));
        assert_eq!(sanitize_next("/").as_deref(), Some("/"));
        assert_eq!(sanitize_next("").as_deref(), Some("/"));
        assert_eq!(
            sanitize_next("/docs/a?b=c#d").as_deref(),
            Some("/docs/a?b=c#d")
        );
    }

    #[test]
    fn sanitize_next_rejects_off_site_redirects() {
        assert!(sanitize_next("https://evil.example.com").is_none());
        assert!(sanitize_next("//evil.example.com").is_none());
        assert!(sanitize_next("/\\evil.example.com").is_none());
        assert!(sanitize_next("/path\\to").is_none());
        assert!(sanitize_next("javascript:alert(1)").is_none());
        assert!(sanitize_next("/a\nSet-Cookie: x=1").is_none());
        assert!(sanitize_next("hr/comp.md").is_none());
    }

    #[test]
    fn missing_credentials_name_the_variable_and_the_docs() {
        // Exercises the lookup directly with a name nothing sets and no stored file.
        //
        // Deliberately not `Credentials::from_env()`: that consults the real
        // ~/.config/mdshelf/credentials.env, so on a machine where somebody has run
        // `mdshelf auth setup` it would find live credentials and the test would
        // report a pass or failure based on the developer's home directory (US-1).
        let error = env_or("MDSHELF_DEFINITELY_NOT_SET_IN_ANY_ENVIRONMENT", None)
            .expect_err("an unset variable with no stored fallback must be an error")
            .to_string();

        assert!(error.contains("MDSHELF_DEFINITELY_NOT_SET_IN_ANY_ENVIRONMENT"));
        assert!(error.contains(DOCS_URL), "got: {error}");
        assert!(error.contains("mdshelf auth setup"), "got: {error}");
    }

    #[test]
    fn a_stored_credentials_file_satisfies_the_lookup() {
        let mut stored = HashMap::new();
        stored.insert(CLIENT_ID_ENV.to_string(), "id-from-file".to_string());
        assert_eq!(
            env_or(CLIENT_ID_ENV, Some(&stored)).unwrap(),
            "id-from-file"
        );

        // A blank value in the file is treated as absent rather than accepted.
        let mut blank = HashMap::new();
        blank.insert(CLIENT_ID_ENV.to_string(), "   ".to_string());
        assert!(env_or("MDSHELF_ALSO_NOT_SET_ANYWHERE", Some(&blank)).is_err());
    }

    #[test]
    fn env_file_parsing_ignores_comments_and_blanks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.env");
        std::fs::write(
            &path,
            "# a comment\n\n  MDSHELF_GOOGLE_CLIENT_ID = id-value \n\
             MDSHELF_GOOGLE_CLIENT_SECRET=\"quoted-secret\"\nnot-a-pair\n",
        )
        .unwrap();

        let values = read_env_file(&path).expect("file parses");
        assert_eq!(values.get(CLIENT_ID_ENV).unwrap(), "id-value");
        assert_eq!(values.get(CLIENT_SECRET_ENV).unwrap(), "quoted-secret");
        assert!(!values.contains_key("not-a-pair"));
    }

    #[test]
    fn redacted_session_id_reveals_only_a_prefix() {
        let redacted = redact_session_id("abcdefghijklmnop");
        assert_eq!(redacted, "abcdef…");
        assert!(!redacted.contains("ghij"));
    }

    #[test]
    fn expired_flows_are_pruned() {
        let mut flows = HashMap::new();
        flows.insert(
            "fresh".to_string(),
            PendingFlow {
                verifier: "v".into(),
                nonce: "n".into(),
                next: "/".into(),
                created_at: now_ms(),
            },
        );
        flows.insert(
            "stale".to_string(),
            PendingFlow {
                verifier: "v".into(),
                nonce: "n".into(),
                next: "/".into(),
                created_at: now_ms() - (FLOW_TTL.as_millis() as i64) - 1_000,
            },
        );
        prune_expired_flows(&mut flows);
        assert!(flows.contains_key("fresh"));
        assert!(!flows.contains_key("stale"));
    }

    #[test]
    fn settings_build_redirect_uri_and_cookie_policy() {
        let settings = AuthSettings {
            session_max_age: Duration::from_secs(60),
            audit_retention: Duration::from_secs(60),
            owner_email: None,
            public_url: "https://docs.acme.com/".to_string(),
            database_path: PathBuf::from("/tmp/mdshelf.db"),
            key_file_path: PathBuf::from("/tmp/secret.key"),
        };
        // The trailing slash must not survive into the redirect URI: Google matches it
        // as an exact string.
        assert_eq!(
            settings.redirect_uri(),
            "https://docs.acme.com/auth/callback"
        );
        assert!(settings.is_secure_origin());

        let local = AuthSettings {
            public_url: "http://127.0.0.1:4444".to_string(),
            ..settings
        };
        assert!(!local.is_secure_origin());
        assert_eq!(local.redirect_uri(), "http://127.0.0.1:4444/auth/callback");
    }
}
