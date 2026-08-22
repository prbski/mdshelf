mod error;
mod livereload;
pub mod routes;
pub mod tls;

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::sync::{RwLock, broadcast};
use tracing::info;

use crate::auth::AuthRuntime;
use crate::cli::ServeArgs;
use crate::config::Config;
use crate::content::Universe;
use crate::render::Renderer;
use crate::render::markdown::MarkdownRenderer;
use crate::theme::ThemeStack;

/// Decide whether auth is on, and if so build its runtime.
///
/// Auth is enabled by `--auth google`. Any failure here aborts startup rather than
/// letting the server come up in a state the operator did not ask for (US-1).
async fn build_auth_runtime(
    config: &Config,
    args: &ServeArgs,
    public_url: &str,
) -> Result<Option<Arc<AuthRuntime>>> {
    let Some(provider) = args.auth.as_deref() else {
        // NFR-2: no --auth flag means nothing about the old behaviour changes, even if
        // the config happens to carry an [auth] section.
        return Ok(None);
    };
    if provider != "google" {
        bail!("--auth {provider} is not supported; only `google` is available.");
    }

    let auth_config = config.auth.clone().unwrap_or_default();
    let runtime = AuthRuntime::initialize(config, &auth_config, public_url.to_string())
        .await
        .context("initializing Google authentication")?;
    info!(
        redirect_uri = %runtime.settings.redirect_uri(),
        "Google authentication enabled"
    );
    Ok(Some(Arc::new(runtime)))
}

/// How often the access log is swept for entries past their retention window.
const AUDIT_PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Prune the access log on startup and hourly thereafter (D27, US-21).
///
/// The log is personal data with a stated retention period, so the sweep has to run on
/// its own rather than waiting for someone to invoke a command.
fn spawn_audit_pruner(state: &Arc<AppState>) {
    if state.auth.is_none() {
        return;
    }
    let state = Arc::downgrade(state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(AUDIT_PRUNE_INTERVAL);
        loop {
            ticker.tick().await;
            let Some(state) = state.upgrade() else {
                return;
            };
            let Some(runtime) = state.auth.clone() else {
                return;
            };
            // rusqlite is blocking; keep it off the async worker threads.
            let _ = tokio::task::spawn_blocking(move || runtime.prune_audit()).await;
        }
    });
}

/// Refresh the derived rule index in the sidecar from the vault (D13, US-12).
///
/// This mirrors the in-memory rules into SQLite for inspection and diagnostics. It is
/// pure derived state: request-time resolution reads the in-memory index, so a failure
/// here degrades tooling, never authorization.
pub fn sync_rule_index(state: &AppState, universe: &Universe) {
    let Some(auth) = state.auth.as_ref() else {
        return;
    };
    for site in universe.sites() {
        let rows = site.acl().rows();
        if let Err(error) = auth.store.replace_rules_for_site(&site.mount, &rows) {
            tracing::warn!(
                %error,
                mount = %site.mount,
                "failed to refresh the derived rule index; authorization is unaffected"
            );
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub universe: Arc<RwLock<Universe>>,
    pub renderer: Arc<RwLock<Renderer>>,
    pub markdown: Arc<MarkdownRenderer>,
    pub live_reload_tx: broadcast::Sender<()>,
    pub live_reload_enabled: bool,
    /// `None` means the server is unauthenticated and behaves exactly as it did before
    /// auth existed (NFR-2).
    pub auth: Option<Arc<AuthRuntime>>,
}

impl AppState {
    /// Whether authorization is enforced on this server.
    pub fn auth_enabled(&self) -> bool {
        self.auth.is_some()
    }
}

pub async fn run(args: ServeArgs) -> Result<()> {
    let mut config = Config::load(args.config.as_deref())?;
    if let Some(host) = args.host.clone() {
        config.host = host;
    }
    if let Some(port) = args.port {
        config.port = port;
    }
    let live_reload_enabled = config.server.live_reload && !args.no_live_reload;

    // Resolve TLS before anything else: an unsafe combination should fail before the
    // vault is scanned or a database is created.
    let tls_mode = tls::resolve(&config, &args)?;
    let public_url = tls::public_url(&config, &args, &tls_mode, config.port)?;

    let auth = build_auth_runtime(&config, &args, &public_url).await?;

    let theme = ThemeStack::from_config(&config)?;
    let renderer = Renderer::new(&theme)?;
    let universe = Universe::build(&config)?;
    let markdown = Arc::new(MarkdownRenderer::new());

    let (live_reload_tx, _) = broadcast::channel(32);
    let state = Arc::new(AppState {
        config: Arc::new(config),
        universe: Arc::new(RwLock::new(universe)),
        renderer: Arc::new(RwLock::new(renderer)),
        markdown,
        live_reload_tx,
        live_reload_enabled,
        auth,
    });

    {
        let universe = state.universe.read().await;
        sync_rule_index(&state, &universe);
    }
    spawn_audit_pruner(&state);

    livereload::spawn_watcher(state.clone()).await?;

    let application_router = routes::router(state.clone());
    let bind_address: std::net::SocketAddr = state
        .config
        .bind_addr()
        .parse()
        .with_context(|| format!("parsing bind address {}", state.config.bind_addr()))?;
    info!("listening on {} ({})", public_url, tls_mode.describe());
    tls::serve(tls_mode, bind_address, application_router).await
}
