use std::{
    env,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use once_cell::sync::Lazy;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

static DEFAULT_ENDPOINT: Lazy<Option<String>> = Lazy::new(|| {
    if let Ok(value) = env::var("PLAYTIME_LEADERBOARD_URL") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if let Some(value) = option_env!("LEADERBOARD_DEFAULT_URL") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderboardEntry {
    pub username: String,
    pub total_minutes: f64,
}

#[derive(Debug, Clone, Serialize)]
struct SubmitPayload {
    username: String,
    total_minutes: f64,
}

#[derive(Clone)]
pub enum LeaderboardClient {
    Remote {
        client: Client,
        endpoint: Arc<str>,
    },
    Local {
        path: Arc<PathBuf>,
    },
}

impl LeaderboardClient {
    pub fn auto(data_dir: &Path) -> Result<Self> {
        if let Some(endpoint) = DEFAULT_ENDPOINT.as_ref().cloned() {
            let client = Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .context("Failed to build HTTP client for leaderboard")?;
            return Ok(Self::Remote {
                client,
                endpoint: Arc::from(endpoint.into_boxed_str()),
            });
        }

        let path = data_dir.join("leaderboard.json");
        if !path.exists() {
            fs::write(&path, "[]").context("Failed to initialize local leaderboard storage")?;
        }
        Ok(Self::Local {
            path: Arc::new(path),
        })
    }

    pub fn submit_total_minutes(&self, username: &str, total_minutes: f64) -> Result<()> {
        if username.trim().is_empty() {
            return Err(anyhow!("Username required to sync leaderboard"));
        }

        match self {
            LeaderboardClient::Remote { client, endpoint } => {
                let url = format!("{}/submit", endpoint.trim_end_matches('/'));
                let payload = SubmitPayload {
                    username: username.trim().to_string(),
                    total_minutes,
                };
                let response = client
                    .post(url)
                    .json(&payload)
                    .send()
                    .context("Failed to reach leaderboard service")?;
                if !response.status().is_success() {
                    return Err(anyhow!(
                        "Leaderboard sync failed with status {}",
                        response.status()
                    ));
                }
                Ok(())
            }
            LeaderboardClient::Local { path } => {
                let mut entries = read_local_entries(path)?;
                update_local_entries(&mut entries, username, total_minutes);
                store_local_entries(path, &entries)?;
                Ok(())
            }
        }
    }

    pub fn fetch_top_entries(&self) -> Result<Vec<LeaderboardEntry>> {
        match self {
            LeaderboardClient::Remote { client, endpoint } => {
                let url = format!("{}/top", endpoint.trim_end_matches('/'));
                let response = client
                    .get(url)
                    .send()
                    .context("Failed to query leaderboard service")?
                    .error_for_status()
                    .context("Leaderboard service returned an error status")?;
                let entries: Vec<LeaderboardEntry> = response
                    .json()
                    .context("Failed to parse leaderboard response")?;
                Ok(entries)
            }
            LeaderboardClient::Local { path } => read_local_entries(path),
        }
    }
}

fn read_local_entries(path: &Path) -> Result<Vec<LeaderboardEntry>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let entries: Vec<LeaderboardEntry> = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(entries)
}

fn store_local_entries(path: &Path, entries: &[LeaderboardEntry]) -> Result<()> {
    let payload = serde_json::to_string_pretty(entries)?;
    fs::write(path, payload)
        .with_context(|| format!("Failed to write {}", path.display()))
}

pub fn update_local_entries(
    entries: &mut Vec<LeaderboardEntry>,
    username: &str,
    total_minutes: f64,
) {
    if let Some(existing) = entries
        .iter_mut()
        .find(|entry| entry.username.eq_ignore_ascii_case(username))
    {
        existing.total_minutes = total_minutes;
    } else {
        entries.push(LeaderboardEntry {
            username: username.trim().to_string(),
            total_minutes,
        });
    }
    entries.sort_by(|a, b| b.total_minutes.partial_cmp(&a.total_minutes).unwrap());
    entries.truncate(25);
}
