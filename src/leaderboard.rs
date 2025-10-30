use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use reqwest::{Url, blocking::Client};
use serde::{Deserialize, Serialize};

fn normalize_endpoint(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn runtime_default_endpoint() -> Option<String> {
    env::var("PLAYTIME_LEADERBOARD_URL")
        .ok()
        .and_then(|value| normalize_endpoint(&value))
}

fn baked_in_default_endpoint() -> Option<String> {
    option_env!("LEADERBOARD_DEFAULT_URL").and_then(normalize_endpoint)
}

const FALLBACK_GLOBAL_ENDPOINT: &str = "https://playtracker.al1e.dev";

fn fallback_global_endpoint() -> Option<String> {
    Some(FALLBACK_GLOBAL_ENDPOINT.to_string())
}

fn global_remote_endpoint() -> Option<String> {
    baked_in_default_endpoint()
        .or_else(runtime_default_endpoint)
        .or_else(fallback_global_endpoint)
}

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
        secondary: Option<Arc<str>>,
    },
    Local {
        path: Arc<PathBuf>,
    },
}

impl LeaderboardClient {
    pub fn auto(data_dir: &Path, override_endpoint: Option<&str>) -> Result<Self> {
        if let Some(raw_override) = override_endpoint {
            let trimmed = raw_override.trim();
            if trimmed.eq_ignore_ascii_case("default") || trimmed.eq_ignore_ascii_case("builtin") {
                if let Some(endpoint) = global_remote_endpoint() {
                    return build_remote_client(endpoint, None);
                }
                return build_local_client(data_dir);
            }

            if trimmed.eq_ignore_ascii_case("local") {
                return build_local_client(data_dir);
            }

            if let Some(endpoint) = normalize_endpoint(trimmed) {
                let mut secondary = None;
                if let Some(global) = global_remote_endpoint() {
                    if !global.eq_ignore_ascii_case(endpoint.as_str()) {
                        secondary = Some(global);
                    }
                }
                return build_remote_client(endpoint, secondary);
            }
        }

        if let Some(endpoint) = global_remote_endpoint() {
            return build_remote_client(endpoint, None);
        }

        build_local_client(data_dir)
    }

    pub fn submit_total_minutes(&self, username: &str, total_minutes: f64) -> Result<()> {
        if username.trim().is_empty() {
            return Err(anyhow!("Username required to sync leaderboard"));
        }

        match self {
            LeaderboardClient::Remote {
                client,
                endpoint,
                secondary,
            } => {
                let payload = SubmitPayload {
                    username: username.trim().to_string(),
                    total_minutes,
                };

                let mut errors = Vec::new();
                if let Err(err) = submit_payload(client, endpoint, &payload) {
                    errors.push(err);
                }

                if let Some(secondary) = secondary {
                    if let Err(err) = submit_payload(client, secondary, &payload) {
                        errors.push(err);
                    }
                }

                if !errors.is_empty() {
                    let combined = errors
                        .into_iter()
                        .map(|err| err.to_string())
                        .collect::<Vec<_>>()
                        .join(" | ");
                    return Err(anyhow!(combined));
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
            LeaderboardClient::Remote {
                client, endpoint, ..
            } => {
                let url = build_endpoint_url(endpoint, "top")?;
                let response = client
                    .get(url)
                    .send()
                    .context("Failed to query leaderboard service")?
                    .error_for_status()
                    .context("Leaderboard service returned an error status")?;
                let payload: LeaderboardResponse = response
                    .json()
                    .context("Failed to parse leaderboard response")?;
                Ok(match payload {
                    LeaderboardResponse::Entries(entries) => entries,
                    LeaderboardResponse::Wrapped { entries, .. } => entries,
                })
            }
            LeaderboardClient::Local { path } => read_local_entries(path),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LeaderboardResponse {
    Entries(Vec<LeaderboardEntry>),
    Wrapped {
        entries: Vec<LeaderboardEntry>,
        #[allow(dead_code)]
        leaderboard: Option<String>,
        #[allow(dead_code)]
        generated_at: Option<String>,
        #[allow(dead_code)]
        count: Option<usize>,
    },
}

fn build_remote_client(endpoint: String, secondary: Option<String>) -> Result<LeaderboardClient> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("Failed to build HTTP client for leaderboard")?;
    Ok(LeaderboardClient::Remote {
        client,
        endpoint: Arc::from(endpoint.into_boxed_str()),
        secondary: secondary.map(|s| Arc::from(s.into_boxed_str())),
    })
}

fn build_local_client(data_dir: &Path) -> Result<LeaderboardClient> {
    let path = data_dir.join("leaderboard.json");
    if !path.exists() {
        fs::write(&path, "[]").context("Failed to initialize local leaderboard storage")?;
    }
    Ok(LeaderboardClient::Local {
        path: Arc::new(path),
    })
}

fn submit_payload(client: &Client, endpoint: &Arc<str>, payload: &SubmitPayload) -> Result<()> {
    let url = build_endpoint_url(endpoint, "submit")?;
    let response = client
        .post(url)
        .json(payload)
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

fn build_endpoint_url(base: &Arc<str>, segment: &str) -> Result<Url> {
    let mut url =
        Url::parse(base).with_context(|| format!("Invalid leaderboard endpoint '{}'", base))?;
    {
        let trimmed = segment.trim_matches('/');
        if trimmed.is_empty() {
            return Ok(url);
        }
        let mut segments = url.path_segments_mut().map_err(|_| {
            anyhow!(
                "Leaderboard endpoint '{}' cannot accept path segments",
                base
            )
        })?;
        segments.pop_if_empty();
        segments.push(trimmed);
    }
    Ok(url)
}

fn read_local_entries(path: &Path) -> Result<Vec<LeaderboardEntry>> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let entries: Vec<LeaderboardEntry> = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(entries)
}

fn store_local_entries(path: &Path, entries: &[LeaderboardEntry]) -> Result<()> {
    let payload = serde_json::to_string_pretty(entries)?;
    fs::write(path, payload).with_context(|| format!("Failed to write {}", path.display()))
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
