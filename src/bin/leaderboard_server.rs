use std::{
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use star_citizen_playtime::leaderboard::{update_local_entries, LeaderboardEntry};
use tokio::{fs, net::TcpListener, sync::RwLock};

struct LeaderboardState {
    entries: RwLock<Vec<LeaderboardEntry>>,
    path: PathBuf,
}

impl LeaderboardState {
    async fn load(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("Failed to create {}", parent.display()))?;
            }
        }

        let entries = if fs::try_exists(&path).await.unwrap_or(false) {
            match fs::read(&path).await {
                Ok(bytes) => match serde_json::from_slice::<Vec<LeaderboardEntry>>(&bytes) {
                    Ok(mut list) => {
                        list.sort_by(|a, b| b.total_minutes.partial_cmp(&a.total_minutes).unwrap());
                        list.truncate(25);
                        list
                    }
                    Err(err) => {
                        eprintln!(
                            "Failed to parse existing leaderboard {}: {err}",
                            path.display()
                        );
                        Vec::new()
                    }
                },
                Err(err) => {
                    eprintln!(
                        "Failed to read existing leaderboard {}: {err}",
                        path.display()
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let state = Self {
            entries: RwLock::new(entries),
            path,
        };
        state.persist().await?;
        Ok(state)
    }

    async fn submit(&self, username: String, total_minutes: f64) -> Result<()> {
        {
            let mut guard = self.entries.write().await;
            update_local_entries(&mut guard, &username, total_minutes);
        }
        self.persist().await
    }

    async fn top(&self) -> Vec<LeaderboardEntry> {
        self.entries.read().await.clone()
    }

    async fn persist(&self) -> Result<()> {
        let guard = self.entries.read().await;
        let payload = serde_json::to_vec_pretty(&*guard)?;
        fs::write(&self.path, payload)
            .await
            .with_context(|| format!("Failed to write {}", self.path.display()))
    }
}

#[derive(Deserialize)]
struct SubmitPayload {
    username: String,
    total_minutes: f64,
}

type SharedState = Arc<LeaderboardState>;

type AppResult<T> = Result<T, (StatusCode, String)>;

#[tokio::main]
async fn main() -> Result<()> {
    let addr: SocketAddr = env::var("LEADERBOARD_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
        .parse()
        .context("Invalid LEADERBOARD_ADDR value")?;
    let data_path = PathBuf::from(env::var("LEADERBOARD_STORE").unwrap_or_else(|_| "leaderboard-data.json".to_string()));

    let state = Arc::new(LeaderboardState::load(data_path).await?);

    let app = Router::new()
        .route("/submit", post(submit_handler))
        .route("/top", get(top_handler))
        .with_state(state.clone());

    println!(
        "Leaderboard service listening on http://{} (storage: {})",
        addr,
        state.path.display()
    );

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;
    axum::serve(listener, app)
        .await
        .context("Leaderboard server crashed")?;

    Ok(())
}

async fn submit_handler(
    State(state): State<SharedState>,
    Json(payload): Json<SubmitPayload>,
) -> AppResult<impl IntoResponse> {
    let username = payload.username.trim();
    if username.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Username is required".into()));
    }
    if !payload.total_minutes.is_finite() || payload.total_minutes < 0.0 {
        return Err((StatusCode::BAD_REQUEST, "total_minutes must be a non-negative number".into()));
    }

    state
        .submit(username.to_string(), payload.total_minutes)
        .await
        .map_err(internal_error)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn top_handler(State(state): State<SharedState>) -> AppResult<impl IntoResponse> {
    let entries = state.top().await;
    Ok(Json(entries))
}

fn internal_error(err: anyhow::Error) -> (StatusCode, String) {
    eprintln!("Leaderboard error: {err:?}");
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}
