//! A stand-in for qmd's HTTP `/query` endpoint.
//!
//! qmd ranks for nobody in particular, so what matters in tests is what mdshelf does
//! with a ranking — above all, that a page the reader may not open never comes back.
//! The mock answers every query with the ranking the test set, which lets a test put a
//! restricted page first and check that it is gone.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde_json::{Value, json};

#[derive(Default)]
struct QmdState {
    ranking: Mutex<Vec<String>>,
    failing: Mutex<bool>,
    queries: Mutex<u32>,
}

pub struct MockQmd {
    pub base_url: String,
    state: Arc<QmdState>,
}

impl MockQmd {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binding the mock qmd");
        let addr: SocketAddr = listener.local_addr().expect("mock qmd address");
        let state = Arc::new(QmdState::default());
        let router = Router::new()
            .route("/query", post(query))
            .with_state(Arc::clone(&state));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Self {
            base_url: format!("http://{addr}"),
            state,
        }
    }

    /// The files every query is answered with, best first, as qmd spells them
    /// (`qmd://<collection>/<percent-encoded path>`).
    pub fn rank(&self, files: &[&str]) {
        *lock(&self.state.ranking) = files.iter().map(|file| file.to_string()).collect();
    }

    /// Answer every query with a server error, as qmd does when it cannot search.
    pub fn fail(&self) {
        *lock(&self.state.failing) = true;
    }

    /// How many queries reached the mock.
    pub fn queries(&self) -> u32 {
        *lock(&self.state.queries)
    }
}

async fn query(
    State(state): State<Arc<QmdState>>,
    Json(_body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    *lock(&state.queries) += 1;
    if *lock(&state.failing) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "unavailable" })),
        );
    }
    let results: Vec<Value> = lock(&state.ranking)
        .iter()
        .enumerate()
        .map(|(rank, file)| json!({ "file": file, "score": 1.0 / (rank as f64 + 1.0) }))
        .collect();
    (StatusCode::OK, Json(json!({ "results": results })))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
