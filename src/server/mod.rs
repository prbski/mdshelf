mod error;
mod livereload;
mod routes;

use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, broadcast};
use tracing::info;

use crate::cli::ServeArgs;
use crate::config::Config;
use crate::content::Universe;
use crate::render::Renderer;
use crate::render::markdown::MarkdownRenderer;
use crate::theme::ThemeStack;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub universe: Arc<RwLock<Universe>>,
    pub renderer: Arc<RwLock<Renderer>>,
    pub markdown: Arc<MarkdownRenderer>,
    pub live_reload_tx: broadcast::Sender<()>,
    pub live_reload_enabled: bool,
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
    });

    livereload::spawn_watcher(state.clone()).await?;

    let application_router = routes::router(state.clone());
    let bind_address = state.config.bind_addr();
    let listener = TcpListener::bind(&bind_address).await?;
    info!("listening on http://{}", bind_address);
    axum::serve(listener, application_router).await?;
    Ok(())
}
