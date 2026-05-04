use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::{extract::State, response::IntoResponse};
use futures::{SinkExt, StreamExt};
use notify_debouncer_full::{DebounceEventResult, new_debouncer, notify::RecursiveMode};
use tokio::sync::broadcast;
use tracing::{error, warn};

use crate::config::Config;
use crate::content::Universe;
use crate::render::Renderer;
use crate::server::AppState;
use crate::theme::ThemeStack;

const RELOAD_MESSAGE: &str = "reload";
const PING_INTERVAL: Duration = Duration::from_secs(20);

pub async fn livereload_ws(
    State(state): State<std::sync::Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if !state.live_reload_enabled {
        return (axum::http::StatusCode::NOT_FOUND, "live reload disabled").into_response();
    }
    let receiver = state.live_reload_tx.subscribe();
    ws.on_upgrade(move |socket| handle_socket(socket, receiver))
        .into_response()
}

async fn handle_socket(socket: WebSocket, mut reload_rx: broadcast::Receiver<()>) {
    let (mut sender, mut receiver) = socket.split();
    let mut ping_ticker = tokio::time::interval(PING_INTERVAL);
    ping_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            reload_event = reload_rx.recv() => {
                match reload_event {
                    Ok(()) => {
                        if sender.send(Message::Text(RELOAD_MESSAGE.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = ping_ticker.tick() => {
                if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            client_message = receiver.next() => {
                match client_message {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

pub async fn spawn_watcher(state: std::sync::Arc<AppState>) -> Result<()> {
    let mut paths: Vec<PathBuf> = state.config.sites.iter().map(|s| s.path.clone()).collect();
    paths.extend(state.renderer.read().await.theme().watch_dirs());

    let tx = state.live_reload_tx.clone();
    let handle = tokio::runtime::Handle::current();
    let state_weak = std::sync::Arc::downgrade(&state);
    let config = state.config.clone();

    thread::spawn(move || {
        let handler = move |result: DebounceEventResult| {
            if let Err(errors) = &result {
                for err in errors {
                    warn!(?err, "notify debouncer error");
                }
                return;
            }
            let Ok(events) = result else {
                return;
            };
            if events.is_empty() {
                return;
            }
            let Some(state) = state_weak.upgrade() else {
                return;
            };
            let cfg = config.clone();
            let reload_tx = tx.clone();
            handle.spawn(async move {
                if let Err(err) = rebuild_content(&state, &cfg).await {
                    error!(?err, "failed to rebuild after file change");
                    return;
                }
                let _ = reload_tx.send(());
            });
        };

        let mut debouncer = match new_debouncer(Duration::from_millis(200), None, handler) {
            Ok(d) => d,
            Err(err) => {
                error!(?err, "failed to start file watcher debouncer");
                return;
            }
        };

        for path in paths {
            if let Err(err) = debouncer.watch(path.as_path(), RecursiveMode::Recursive) {
                error!(path = %path.display(), ?err, "failed to watch path");
            }
        }

        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    });

    Ok(())
}

async fn rebuild_content(
    state: &std::sync::Arc<AppState>,
    config: &std::sync::Arc<Config>,
) -> Result<()> {
    let config = config.clone();
    let rebuild_result = tokio::task::spawn_blocking(move || {
        let theme = ThemeStack::from_config(config.as_ref())?;
        let renderer = Renderer::new(&theme)?;
        let universe = Universe::build(config.as_ref())?;
        Ok::<_, anyhow::Error>((universe, renderer))
    })
    .await
    .map_err(|join_error| anyhow::anyhow!(join_error))?;
    let (universe, renderer) = rebuild_result?;
    *state.universe.write().await = universe;
    *state.renderer.write().await = renderer;
    Ok(())
}
