use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::{extract::State, response::IntoResponse};
use futures::{SinkExt, StreamExt};
use notify_debouncer_full::{DebounceEventResult, new_debouncer, notify::RecursiveMode};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, warn};

use crate::config::Config;
use crate::content::Universe;
use crate::content::source::should_trigger_rebuild;
use crate::render::Renderer;
use crate::server::AppState;
use crate::theme::ThemeStack;

const RELOAD_MESSAGE: &str = "reload";
const PING_INTERVAL: Duration = Duration::from_secs(20);

pub async fn livereload_ws(
    State(state): State<Arc<AppState>>,
    jar: axum_extra::extract::cookie::CookieJar,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if !state.live_reload_enabled {
        return (axum::http::StatusCode::NOT_FOUND, "live reload disabled").into_response();
    }

    // US-19. The socket is a long-lived channel into the server, so it must be held to
    // the same standard as a page request: no session, no socket.
    //
    // Reload events carry no payload beyond "something changed" — there is no path in
    // them to filter — so gating the upgrade is the whole of what there is to enforce.
    // If these events ever grow a path, that payload must be filtered per socket
    // against the viewer's ACL before it is sent.
    if state.auth_enabled() {
        let authorized = match jar.get(crate::auth::SESSION_COOKIE) {
            Some(cookie) => {
                let runtime = state.auth.as_ref().expect("auth enabled");
                matches!(
                    runtime.resolve_session(cookie.value()).await,
                    crate::auth::SessionOutcome::Active(_)
                )
            }
            None => false,
        };
        if !authorized {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "sign in to receive live reload events",
            )
                .into_response();
        }
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

pub async fn spawn_watcher(state: Arc<AppState>) -> Result<()> {
    let site_roots: Vec<PathBuf> = state
        .config
        .sites
        .iter()
        .map(|site| site.path.clone())
        .collect();
    let theme_dirs = state.renderer.read().await.theme().watch_dirs();
    let mut watch_paths = site_roots.clone();
    watch_paths.extend(theme_dirs.clone());

    let reload_tx = state.live_reload_tx.clone();
    let handle = tokio::runtime::Handle::current();
    let state_weak = Arc::downgrade(&state);
    let config = state.config.clone();
    let (rebuild_trigger_tx, rebuild_trigger_rx) = mpsc::unbounded_channel();

    handle.spawn(rebuild_worker(
        state_weak.clone(),
        config.clone(),
        reload_tx.clone(),
        rebuild_trigger_rx,
    ));

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
            let relevant_change = events.iter().any(|event| {
                if event.need_rescan() {
                    return true;
                }
                event
                    .paths
                    .iter()
                    .any(|path| should_trigger_rebuild(path, &event.kind, &site_roots, &theme_dirs))
            });
            if !relevant_change {
                return;
            }
            let _ = rebuild_trigger_tx.send(());
        };

        let mut debouncer = match new_debouncer(Duration::from_millis(200), None, handler) {
            Ok(debouncer) => debouncer,
            Err(err) => {
                error!(?err, "failed to start file watcher debouncer");
                return;
            }
        };

        for path in watch_paths {
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

async fn rebuild_worker(
    state_weak: std::sync::Weak<AppState>,
    config: Arc<Config>,
    reload_tx: broadcast::Sender<()>,
    mut rebuild_trigger_rx: mpsc::UnboundedReceiver<()>,
) {
    while rebuild_trigger_rx.recv().await.is_some() {
        loop {
            while rebuild_trigger_rx.try_recv().is_ok() {}

            let Some(state) = state_weak.upgrade() else {
                return;
            };
            if let Err(err) = rebuild_content(&state, &config).await {
                error!(?err, "failed to rebuild after file change");
            } else {
                let _ = reload_tx.send(());
            }

            if rebuild_trigger_rx.try_recv().is_err() {
                break;
            }
        }
    }
}

async fn rebuild_content(state: &Arc<AppState>, config: &Arc<Config>) -> Result<()> {
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
    // Refresh the derived rule index before publishing the new universe, so a request
    // arriving immediately after cannot observe stale rules (US-12).
    crate::server::sync_rule_index(state, &universe);
    *state.universe.write().await = universe;
    *state.renderer.write().await = renderer;
    Ok(())
}
