use std::{
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::Local;
use sysinfo::System;

use crate::storage::{active_session_minutes, format_duration, ActiveSession, Session, SessionStore};

const PROCESS_TOKENS: [&str; 4] = [
    "starcitizen",
    "star citizen",
    "starcitizen64",
    "robertsspaceindustries",
];

pub struct Monitor {
    poll_interval: Duration,
    min_session_minutes: u64,
    snapshot: Option<Arc<Mutex<MonitorSnapshot>>>,
}

impl Monitor {
    pub fn new(poll_interval: Duration, min_session_minutes: u64) -> Self {
        Self {
            poll_interval,
            min_session_minutes,
            snapshot: None,
        }
    }

    pub fn with_status_sink(mut self, snapshot: Arc<Mutex<MonitorSnapshot>>) -> Self {
        self.snapshot = Some(snapshot);
        self
    }

    pub fn run(&mut self, stop: Arc<AtomicBool>) -> Result<()> {
        let mut system = System::new();
        let store = SessionStore::new()?;
        let mut active = store
            .load_active()
            .context("Failed to restore active session state")?;

        println!(
            "Star Citizen monitor running (poll every {}s, min session {}m)",
            self.poll_interval.as_secs(),
            self.min_session_minutes
        );

        self.update_snapshot(|snapshot| {
            snapshot.status_text = "Idle".to_string();
            snapshot.active_session = active.clone();
        });

        system.refresh_processes();

        if let Some(ref session) = active {
            println!(
                "Resumed active session from {}",
                session.start.format("%Y-%m-%d %H:%M:%S")
            );
            if !is_game_running(&system) {
                if let Some(saved) = finalize_session(&store, session.clone(), self.min_session_minutes)? {
                    println!(
                        "Recovered session saved: {} for {}",
                        saved.start.format("%Y-%m-%d %H:%M:%S"),
                        format_duration(saved.duration_minutes)
                    );
                    active = None;
                    self.update_snapshot(|snapshot| {
                        snapshot.status_text = "Idle".to_string();
                        snapshot.active_session = None;
                        snapshot.last_session = Some(saved);
                    });
                }
            } else {
                self.update_snapshot(|snapshot| {
                    snapshot.status_text = "Tracking".to_string();
                    snapshot.active_session = Some(session.clone());
                });
            }
        }

        loop {
            if stop.load(Ordering::SeqCst) {
                if let Some(active) = active {
                    store.save_active(&active)?;
                    self.update_snapshot(|snapshot| {
                        snapshot.active_session = Some(active);
                        snapshot.status_text = "Pending resume".to_string();
                    });
                }
                println!("Stop flag set, shutting down monitor loop.");
                break;
            }

            system.refresh_processes();
            let running = is_game_running(&system);
            let now = Local::now();

            if running {
                match active {
                    Some(ref mut session) => {
                        session.last_seen = now;
                        store.save_active(session)?;
                        let snapshot_session = session.clone();
                        self.update_snapshot(|snapshot| {
                            snapshot.status_text = "Tracking".to_string();
                            snapshot.active_session = Some(snapshot_session);
                        });
                    }
                    None => {
                        let session = ActiveSession::new(now);
                        store.save_active(&session)?;
                        println!(
                            "Detected Star Citizen start at {}",
                            session.start.format("%Y-%m-%d %H:%M:%S")
                        );
                        self.update_snapshot(|snapshot| {
                            snapshot.status_text = "Tracking".to_string();
                            snapshot.active_session = Some(session.clone());
                        });
                        active = Some(session);
                    }
                }
            } else if let Some(session) = active.take() {
                if let Some(saved) = finalize_session(&store, session, self.min_session_minutes)? {
                    println!(
                        "Session saved: {} lasting {}",
                        saved.start.format("%Y-%m-%d %H:%M:%S"),
                        format_duration(saved.duration_minutes)
                    );
                    self.update_snapshot(|snapshot| {
                        snapshot.status_text = "Idle".to_string();
                        snapshot.active_session = None;
                        snapshot.last_session = Some(saved);
                    });
                } else {
                    self.update_snapshot(|snapshot| {
                        snapshot.status_text = "Idle".to_string();
                        snapshot.active_session = None;
                    });
                }
            } else {
                self.update_snapshot(|snapshot| {
                    if snapshot.active_session.is_some() {
                        snapshot.active_session = None;
                    }
                    snapshot.status_text = "Idle".to_string();
                });
            }

            thread::sleep(self.poll_interval);
        }

        println!("Monitor loop exited normally.");
        Ok(())
    }

    fn update_snapshot<F>(&self, update: F)
    where
        F: FnOnce(&mut MonitorSnapshot),
    {
        if let Some(snapshot) = &self.snapshot {
            if let Ok(mut guard) = snapshot.lock() {
                update(&mut guard);
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MonitorSnapshot {
    pub status_text: String,
    pub active_session: Option<ActiveSession>,
    pub last_session: Option<Session>,
}

fn is_game_running(system: &System) -> bool {
    system.processes().values().any(|process| {
        let name = process.name().to_ascii_lowercase();
        if PROCESS_TOKENS.iter().any(|token| name.contains(token)) {
            return true;
        }
        if let Some(exe) = process.exe() {
            if exe
                .file_name()
                .and_then(|f| f.to_str())
                .map(|s| s.to_ascii_lowercase())
                .map(|s| PROCESS_TOKENS.iter().any(|token| s.contains(token)))
                .unwrap_or(false)
            {
                return true;
            }
        }
        false
    })
}

fn finalize_session(
    store: &SessionStore,
    active: ActiveSession,
    min_session_minutes: u64,
) -> Result<Option<Session>> {
    let minutes = active_session_minutes(&active);
    if minutes < min_session_minutes as f64 {
        store.clear_active()?;
        return Ok(None);
    }
    let session = Session::new(active.start, active.last_seen, String::new());
    store.append_session(session.clone())?;
    store.clear_active()?;
    Ok(Some(session))
}
