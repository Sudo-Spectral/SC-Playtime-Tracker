use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub duration_minutes: f64,
    #[serde(default)]
    pub note: String,
}

impl Session {
    pub fn new(start: DateTime<Local>, end: DateTime<Local>, note: String) -> Self {
        let duration = end - start;
        let duration_minutes = duration.num_seconds().max(0) as f64 / 60.0;
        Self {
            id: Uuid::new_v4(),
            start,
            end,
            duration_minutes,
            note,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSession {
    pub start: DateTime<Local>,
    pub last_seen: DateTime<Local>,
}

impl ActiveSession {
    pub fn new(start: DateTime<Local>) -> Self {
        Self {
            start,
            last_seen: start,
        }
    }
}

pub struct SessionStore {
    data_dir: PathBuf,
    sessions_file: PathBuf,
    active_file: PathBuf,
}

pub struct Analytics {
    pub total_sessions: usize,
    pub total_minutes: f64,
    pub average_session_minutes: f64,
    pub median_session_minutes: f64,
    pub minutes_last_7: f64,
    pub minutes_last_30: f64,
    pub top_days: Vec<(NaiveDate, f64)>,
    pub recent_sessions: Vec<Session>,
    pub recent_daily: Vec<(NaiveDate, f64)>,
    pub recent_weekly: Vec<((i32, u32), f64)>,
    pub first_day: Option<NaiveDate>,
    pub last_day: Option<NaiveDate>,
}

impl SessionStore {
    pub fn new() -> Result<Self> {
        let base_dirs = BaseDirs::new().context("Unable to determine platform data directory")?;
        let mut data_dir = base_dirs.data_local_dir().to_path_buf();
        data_dir.push("StarCitizenPlaytime");
        fs::create_dir_all(&data_dir).context("Failed to create data directory")?;
        let sessions_file = data_dir.join("sessions.json");
        let active_file = data_dir.join("active_session.json");
        Ok(Self {
            data_dir,
            sessions_file,
            active_file,
        })
    }

    pub fn load_sessions(&self) -> Result<Vec<Session>> {
        if !self.sessions_file.exists() {
            return Ok(vec![]);
        }
        let content = fs::read_to_string(&self.sessions_file)
            .with_context(|| format!("Failed to read {}", self.sessions_file.display()))?;
        let sessions: Vec<Session> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", self.sessions_file.display()))?;
        Ok(sessions)
    }

    pub fn save_sessions(&self, sessions: &[Session]) -> Result<()> {
        let mut ordered = sessions.to_vec();
        ordered.sort_by_key(|s| s.start);
        let payload = serde_json::to_string_pretty(&ordered)?;
        fs::write(&self.sessions_file, payload)
            .with_context(|| format!("Failed to write {}", self.sessions_file.display()))
    }

    pub fn append_session(&self, session: Session) -> Result<()> {
        let mut sessions = self.load_sessions()?;
        sessions.push(session);
        self.save_sessions(&sessions)
    }

    pub fn load_active(&self) -> Result<Option<ActiveSession>> {
        if !self.active_file.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&self.active_file)
            .with_context(|| format!("Failed to read {}", self.active_file.display()))?;
        let active: ActiveSession = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", self.active_file.display()))?;
        Ok(Some(active))
    }

    pub fn save_active(&self, active: &ActiveSession) -> Result<()> {
        let payload = serde_json::to_string_pretty(active)?;
        fs::write(&self.active_file, payload)
            .with_context(|| format!("Failed to write {}", self.active_file.display()))
    }

    pub fn clear_active(&self) -> Result<()> {
        if self.active_file.exists() {
            fs::remove_file(&self.active_file)
                .with_context(|| format!("Failed to delete {}", self.active_file.display()))?;
        }
        Ok(())
    }

    pub fn export_csv(&self, path: &Path, sessions: &[Session]) -> Result<(usize, PathBuf)> {
        let mut out_path = path.to_path_buf();
        if out_path.extension().map(|ext| ext != "csv").unwrap_or(true) {
            out_path.set_extension("csv");
        }
        let mut file = fs::File::create(&out_path)
            .with_context(|| format!("Failed to create {}", out_path.display()))?;
        writeln!(file, "id,start,end,duration_minutes,note")?;
        for session in sessions {
            let note = session.note.replace('"', "'");
            writeln!(
                file,
                "{},{},{},{:.2},\"{}\"",
                session.id,
                session.start.to_rfc3339(),
                session.end.to_rfc3339(),
                session.duration_minutes,
                note
            )?;
        }
        Ok((sessions.len(), out_path))
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

pub fn format_duration(minutes: f64) -> String {
    if minutes <= 0.0 {
        return "0m".to_string();
    }
    let total_seconds = (minutes * 60.0) as i64;
    let hours = total_seconds / 3600;
    let mins = (total_seconds % 3600) / 60;
    match (hours, mins) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

pub fn active_session_minutes(active: &ActiveSession) -> f64 {
    let duration = active.last_seen - active.start;
    duration.num_seconds().max(0) as f64 / 60.0
}

pub fn compute_analytics(sessions: &[Session]) -> Analytics {
    use std::collections::BTreeMap;

    let total_sessions = sessions.len();
    let total_minutes: f64 = sessions.iter().map(|s| s.duration_minutes).sum();
    let average_session_minutes = if total_sessions == 0 {
        0.0
    } else {
        total_minutes / total_sessions as f64
    };

    let mut durations: Vec<f64> = sessions.iter().map(|s| s.duration_minutes).collect();
    durations.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median_session_minutes = if durations.is_empty() {
        0.0
    } else {
        let mid = durations.len() / 2;
        if durations.len() % 2 == 0 {
            (durations[mid - 1] + durations[mid]) / 2.0
        } else {
            durations[mid]
        }
    };

    let mut daily_totals: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    for session in sessions {
        let day = session.start.date_naive();
        *daily_totals.entry(day).or_default() += session.duration_minutes;
    }

    let mut weekly_totals: BTreeMap<(i32, u32), f64> = BTreeMap::new();
    for (day, minutes) in &daily_totals {
        let iso_week = day.iso_week();
        *weekly_totals.entry((iso_week.year(), iso_week.week())).or_default() += minutes;
    }

    let recent_sessions = {
        let mut list = sessions.to_vec();
        list.sort_by_key(|s| s.start);
        list.into_iter().rev().take(20).collect::<Vec<_>>()
    };

    let mut top_days = daily_totals
        .iter()
        .map(|(day, minutes)| (*day, *minutes))
        .collect::<Vec<_>>();
    top_days.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    top_days.truncate(5);

    let recent_daily = daily_totals
        .iter()
        .rev()
        .map(|(day, minutes)| (*day, *minutes))
        .take(14)
        .collect();

    let recent_weekly = weekly_totals
        .iter()
        .rev()
        .map(|(week, minutes)| (*week, *minutes))
        .take(8)
        .collect();

    let today = Local::now().date_naive();
    let minutes_last_7: f64 = daily_totals
        .iter()
        .filter(|(day, _)| **day >= today - Duration::days(6))
        .map(|(_, minutes)| *minutes)
        .sum();
    let minutes_last_30: f64 = daily_totals
        .iter()
        .filter(|(day, _)| **day >= today - Duration::days(29))
        .map(|(_, minutes)| *minutes)
        .sum();

    let first_day = daily_totals.keys().next().copied();
    let last_day = daily_totals.keys().next_back().copied();

    Analytics {
        total_sessions,
        total_minutes,
        average_session_minutes,
        median_session_minutes,
        minutes_last_7,
        minutes_last_30,
        top_days,
        recent_sessions,
        recent_daily,
        recent_weekly,
        first_day,
        last_day,
    }
}
