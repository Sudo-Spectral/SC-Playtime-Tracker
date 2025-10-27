use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub poll_seconds: u64,
    pub min_session_minutes: u64,
    pub refresh_seconds: u64,
    pub run_on_login: bool,
    pub show_daily_chart: bool,
    pub show_weekly_chart: bool,
    pub sync_leaderboard: bool,
    pub leaderboard_username: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            poll_seconds: 15,
            min_session_minutes: 3,
            refresh_seconds: 5,
            run_on_login: false,
            show_daily_chart: true,
            show_weekly_chart: true,
            sync_leaderboard: true,
            leaderboard_username: String::new(),
        }
    }
}

impl AppSettings {
    pub fn sanitize(&mut self) {
        self.poll_seconds = self.poll_seconds.clamp(1, 3600);
        self.min_session_minutes = self.min_session_minutes.clamp(1, 1440);
        self.refresh_seconds = self.refresh_seconds.clamp(1, 60);
        if !self.show_daily_chart && !self.show_weekly_chart {
            self.show_daily_chart = true;
        }
        self.leaderboard_username = self.leaderboard_username.trim().to_string();
        if self.leaderboard_username.len() > 32 {
            self.leaderboard_username.truncate(32);
        }
    }
}

#[derive(Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(data_dir: PathBuf) -> Self {
        let path = data_dir.join("settings.json");
        Self { path }
    }

    pub fn load(&self) -> Result<AppSettings> {
        if !self.path.exists() {
            return Ok(AppSettings::default());
        }
        let raw = fs::read_to_string(&self.path)
            .with_context(|| format!("Failed to read {}", self.path.display()))?;
        let mut settings: AppSettings = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse {}", self.path.display()))?;
        settings.sanitize();
        Ok(settings)
    }

    pub fn save(&self, settings: &AppSettings) -> Result<()> {
        let mut normalized = settings.clone();
        normalized.sanitize();
        let payload = serde_json::to_string_pretty(&normalized)?;
        fs::write(&self.path, payload)
            .with_context(|| format!("Failed to write {}", self.path.display()))
    }
}
