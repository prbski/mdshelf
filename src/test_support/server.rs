//! Boots a real mdshelf server in-process against a temporary vault.
//!
//! Tests drive it over HTTP exactly as a browser would, so what is verified is the
//! behaviour of the actual router, middleware, and renderer — not a reimplementation.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;
use tokio::sync::{RwLock, broadcast};

use crate::auth::{AuthRuntime, Credentials};
use crate::config::{AuthConfig, Config, SiteConfig};
use crate::content::Universe;
use crate::render::Renderer;
use crate::render::markdown::MarkdownRenderer;
use crate::server::AppState;
use crate::theme::ThemeStack;

use super::mock_idp::MockIdp;

pub const TEST_CLIENT_ID: &str = "test-client-id";
pub const TEST_CLIENT_SECRET: &str = "test-client-secret";

/// One site in a multi-site test server.
pub struct TestSite<'a> {
    pub mount: &'a str,
    pub title: &'a str,
    pub files: &'a [(&'a str, &'a str)],
}

/// A running server plus the temporary vault behind it.
pub struct TestServer {
    pub base_url: String,
    pub state: Arc<AppState>,
    pub vault: PathBuf,
    /// Held so the temporary directory outlives the server.
    _dir: TempDir,
}

impl TestServer {
    /// Start an unauthenticated server — the pre-auth behaviour (NFR-2).
    pub async fn start_public(files: &[(&str, &str)]) -> Self {
        Self::start_inner(files, None, false).await
    }

    /// Start a server with Google auth enabled against `idp`.
    pub async fn start_with_auth(files: &[(&str, &str)], idp: &MockIdp) -> Self {
        Self::start_inner(files, Some(idp), false).await
    }

    /// As [`TestServer::start_with_auth`], with the live-reload socket enabled.
    pub async fn start_with_auth_and_live_reload(files: &[(&str, &str)], idp: &MockIdp) -> Self {
        Self::start_inner(files, Some(idp), true).await
    }

    /// Start an authenticated server hosting several sites.
    ///
    /// Multi-site is a supported configuration, and several surfaces — the site
    /// switcher above all — only differ across sites, so they cannot be exercised by a
    /// single-site harness.
    pub async fn start_with_auth_sites(sites: &[TestSite<'_>], idp: &MockIdp) -> Self {
        Self::start_inner_sites(sites, Some(idp), false).await
    }

    async fn start_inner(files: &[(&str, &str)], idp: Option<&MockIdp>, live_reload: bool) -> Self {
        let sites = [TestSite {
            mount: "/docs",
            title: "Docs",
            files,
        }];
        Self::start_inner_sites(&sites, idp, live_reload).await
    }

    async fn start_inner_sites(
        sites: &[TestSite<'_>],
        idp: Option<&MockIdp>,
        live_reload: bool,
    ) -> Self {
        let dir = TempDir::new().expect("creating a temporary vault");

        let mut site_configs = Vec::with_capacity(sites.len());
        for site in sites {
            let root = dir.path().join(site.mount.trim_start_matches('/'));
            std::fs::create_dir_all(&root).expect("creating the vault directory");
            write_files(&root, site.files);
            site_configs.push(SiteConfig::for_test_at(&root, site.mount, site.title));
        }
        // The first site's root, for the `vault` convenience accessor.
        let vault = dir.path().join(sites[0].mount.trim_start_matches('/'));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binding the test server");
        let addr: SocketAddr = listener.local_addr().expect("test server address");
        let base_url = format!("http://{addr}");

        let mut config = Config::for_test(dir.path().to_path_buf(), site_configs);
        config.host = "127.0.0.1".to_string();
        config.port = addr.port();

        let auth = match idp {
            Some(idp) => {
                let auth_config = AuthConfig {
                    // Keep all state inside the temporary directory so tests never touch
                    // the developer's real ~/.config/mdshelf.
                    database: Some(dir.path().join("mdshelf.db")),
                    key_file: Some(dir.path().join("secret.key")),
                    owner_email: Some("owner@corp.com".to_string()),
                    ..AuthConfig::default()
                };
                let credentials = Credentials {
                    client_id: TEST_CLIENT_ID.to_string(),
                    client_secret: TEST_CLIENT_SECRET.to_string(),
                };
                idp.set_audience(TEST_CLIENT_ID);
                let runtime = AuthRuntime::build(
                    &config,
                    &auth_config,
                    base_url.clone(),
                    credentials,
                    &idp.discovery_url(),
                )
                .await
                .expect("building the auth runtime");
                Some(Arc::new(runtime))
            }
            None => None,
        };

        let theme = ThemeStack::from_config(&config).expect("loading the theme");
        let renderer = Renderer::new(&theme).expect("building the renderer");
        let universe = Universe::build(&config).expect("building the universe");
        let (live_reload_tx, _) = broadcast::channel(32);

        let state = Arc::new(AppState {
            config: Arc::new(config),
            universe: Arc::new(RwLock::new(universe)),
            renderer: Arc::new(RwLock::new(renderer)),
            markdown: Arc::new(MarkdownRenderer::new()),
            live_reload_tx,
            live_reload_enabled: live_reload,
            auth,
        });

        let router = crate::server::routes::router(Arc::clone(&state));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Self {
            base_url,
            state,
            vault,
            _dir: dir,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Rewrite a file in the vault and rebuild, as the watcher would.
    pub async fn write_and_rebuild(&self, relative: &str, contents: &str) {
        write_files(&self.vault, &[(relative, contents)]);
        self.rebuild().await;
    }

    /// Remove a file from the vault and rebuild.
    pub async fn remove_and_rebuild(&self, relative: &str) {
        std::fs::remove_file(self.vault.join(relative)).expect("removing a vault file");
        self.rebuild().await;
    }

    /// Rebuild content the way the filesystem watcher does.
    pub async fn rebuild(&self) {
        let rebuilt = Universe::build(&self.state.config).expect("rebuilding the universe");
        *self.state.universe.write().await = rebuilt;
    }
}

fn write_files(root: &Path, files: &[(&str, &str)]) {
    for (relative, contents) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("creating a vault subdirectory");
        }
        std::fs::write(&path, contents).expect("writing a vault file");
    }
}

/// An HTTP client that does not follow redirects, so tests can inspect each hop of the
/// OAuth flow, and that carries cookies only when a test sets them explicitly.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("building the test HTTP client")
}
