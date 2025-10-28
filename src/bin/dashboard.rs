#![cfg_attr(windows, windows_subsystem = "windows")]

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use chrono::{Duration as ChronoDuration, Local, NaiveDate};
use eframe::egui::epaint::Shadow;
use eframe::egui::{
    self, Color32, Frame, Grid, Margin, RichText, Rounding, ScrollArea, Stroke, Vec2b,
    ViewportCommand, style::Visuals,
};
use egui_plot::{Bar, BarChart, Legend, Plot, PlotBounds, PlotPoint};
use rfd::FileDialog;
use star_citizen_playtime::leaderboard::{LeaderboardClient, LeaderboardEntry};
use star_citizen_playtime::monitor::{Monitor, MonitorSnapshot};
use star_citizen_playtime::settings::{AppSettings, SettingsStore};
#[cfg(windows)]
use star_citizen_playtime::startup;
use star_citizen_playtime::storage::{
    Analytics, Session, SessionStore, active_session_minutes, compute_analytics, format_duration,
};

#[cfg(windows)]
use std::{env, process::Command};

#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{
        PostMessageW, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow, WM_CLOSE,
    },
};

#[cfg(windows)]
use tray_icon::{
    ClickType, Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuId, MenuItemBuilder, PredefinedMenuItem},
};

#[cfg(windows)]
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, VelopackApp, sources::HttpSource};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum DashboardTab {
    Overview,
    Insights,
}

#[cfg(windows)]
struct TrayController {
    _icon: TrayIcon,
    event_rx: std::sync::mpsc::Receiver<TrayAction>,
    repaint_keepalive: Arc<AtomicBool>,
    _window_handle: Arc<AtomicIsize>,
}

#[cfg(windows)]
impl TrayController {
    fn new(repaint_ctx: &egui::Context, window_handle: Arc<AtomicIsize>) -> Option<Self> {
        Self::log_event("initializing tray controller");

        let show_id = MenuId::new("open_dashboard");
        let exit_id = MenuId::new("quit_dashboard");

        let show_item = MenuItemBuilder::new()
            .id(show_id.clone())
            .text("Open Dashboard")
            .enabled(true)
            .build();
        let exit_item = MenuItemBuilder::new()
            .id(exit_id.clone())
            .text("Quit")
            .enabled(true)
            .build();
        let separator = PredefinedMenuItem::separator();

        let menu = Menu::new();
        if let Err(err) = menu.append(&show_item) {
            Self::log_event(&format!("failed to append show menu item: {err:?}"));
            return None;
        }
        if let Err(err) = menu.append(&separator) {
            Self::log_event(&format!("failed to append separator: {err:?}"));
            return None;
        }
        if let Err(err) = menu.append(&exit_item) {
            Self::log_event(&format!("failed to append exit menu item: {err:?}"));
            return None;
        }

        let icon = match Icon::from_rgba(Self::icon_pixels(), 16, 16) {
            Ok(icon) => icon,
            Err(err) => {
                Self::log_event(&format!("failed to create tray icon image: {err:?}"));
                return None;
            }
        };

        let icon = match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_tooltip("Star Citizen Playtime")
            .build()
        {
            Ok(icon) => icon,
            Err(err) => {
                Self::log_event(&format!("failed to build tray icon: {err:?}"));
                return None;
            }
        };

        let (tx, rx) = std::sync::mpsc::channel();
        let repaint_ctx_icon = repaint_ctx.clone();
        let icon_window_handle = Arc::clone(&window_handle);

        {
            let tx = tx.clone();
            let icon_id = icon.id().clone();
            thread::spawn(move || {
                let tray_events = TrayIconEvent::receiver();
                loop {
                    match tray_events.recv() {
                        Ok(event) => {
                            TrayController::log_event(&format!(
                                "tray icon event (worker): {:?}",
                                event
                            ));
                            if event.id == icon_id
                                && matches!(event.click_type, ClickType::Left | ClickType::Double)
                            {
                                if let Err(err) = tx.send(TrayAction::Show) {
                                    TrayController::log_event(&format!(
                                        "failed to enqueue TrayAction::Show: {err:?}"
                                    ));
                                    break;
                                }
                                TrayController::log_event("enqueued TrayAction::Show from icon");
                                repaint_ctx_icon.request_repaint();
                                TrayController::show_window(&icon_window_handle);
                            }
                        }
                        Err(err) => {
                            TrayController::log_event(&format!(
                                "tray icon event channel closed: {err:?}"
                            ));
                            break;
                        }
                    }
                }
            });
        }

        {
            let tx = tx.clone();
            let show_id = show_id.clone();
            let exit_id = exit_id.clone();
            let repaint_ctx_menu = repaint_ctx.clone();
            let menu_window_handle = Arc::clone(&window_handle);
            thread::spawn(move || {
                let menu_events = MenuEvent::receiver();
                loop {
                    match menu_events.recv() {
                        Ok(event) => {
                            TrayController::log_event(&format!(
                                "menu event (worker): {:?}",
                                event.id
                            ));
                            if event.id == show_id {
                                if let Err(err) = tx.send(TrayAction::Show) {
                                    TrayController::log_event(&format!(
                                        "failed to enqueue TrayAction::Show: {err:?}"
                                    ));
                                    break;
                                }
                                TrayController::log_event("enqueued TrayAction::Show from menu");
                                repaint_ctx_menu.request_repaint();
                                TrayController::show_window(&menu_window_handle);
                            } else if event.id == exit_id {
                                if let Err(err) = tx.send(TrayAction::Exit) {
                                    TrayController::log_event(&format!(
                                        "failed to enqueue TrayAction::Exit: {err:?}"
                                    ));
                                    break;
                                }
                                TrayController::log_event("enqueued TrayAction::Exit from menu");
                                repaint_ctx_menu.request_repaint();
                                TrayController::close_window(&menu_window_handle);
                            }
                        }
                        Err(err) => {
                            TrayController::log_event(&format!(
                                "menu event channel closed: {err:?}"
                            ));
                            break;
                        }
                    }
                }
            });
        }

        let keepalive_flag = Arc::new(AtomicBool::new(true));
        {
            let repaint_ctx_keepalive = repaint_ctx.clone();
            let keepalive_flag_clone = keepalive_flag.clone();
            thread::spawn(move || {
                TrayController::log_event("repaint keepalive thread started");
                while keepalive_flag_clone.load(Ordering::SeqCst) {
                    repaint_ctx_keepalive.request_repaint();
                    thread::sleep(Duration::from_millis(250));
                }
                TrayController::log_event("repaint keepalive thread exiting");
            });
        }

        Self::log_event("tray controller initialized successfully");

        Some(Self {
            _icon: icon,
            event_rx: rx,
            repaint_keepalive: keepalive_flag,
            _window_handle: window_handle,
        })
    }

    fn icon_pixels() -> Vec<u8> {
        let mut data = Vec::with_capacity(16 * 16 * 4);
        for _ in 0..(16 * 16) {
            data.extend_from_slice(&[0x42, 0x7A, 0xC4, 0xFF]);
        }
        data
    }

    fn log_event(msg: &str) {
        let path = std::env::temp_dir().join("sc_playtime_tray.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{} - {}", chrono::Local::now().to_rfc3339(), msg);
        }
    }

    fn hwnd_from(handle: &Arc<AtomicIsize>) -> Option<HWND> {
        let raw = handle.load(Ordering::SeqCst);
        if raw == 0 { None } else { Some(raw as HWND) }
    }

    fn show_window(handle: &Arc<AtomicIsize>) {
        match Self::hwnd_from(handle) {
            Some(hwnd) => unsafe {
                ShowWindow(hwnd, SW_RESTORE);
                ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
            },
            None => Self::log_event("show requested but window handle unavailable"),
        }
    }

    fn close_window(handle: &Arc<AtomicIsize>) {
        match Self::hwnd_from(handle) {
            Some(hwnd) => unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, 0, 0);
            },
            None => Self::log_event("close requested but window handle unavailable"),
        }
    }

    fn drain_actions<F>(&self, mut on_action: F)
    where
        F: FnMut(TrayAction),
    {
        static FIRST_POLL: AtomicBool = AtomicBool::new(true);
        if FIRST_POLL.swap(false, Ordering::SeqCst) {
            Self::log_event("tray controller poll started");
        }

        while let Ok(action) = self.event_rx.try_recv() {
            Self::log_event(&format!("drain_actions received: {:?}", action));
            on_action(action);
        }
    }
}

#[cfg(windows)]
impl Drop for TrayController {
    fn drop(&mut self) {
        self.repaint_keepalive.store(false, Ordering::SeqCst);
        Self::log_event("tray controller dropped");
    }
}

#[cfg(windows)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum TrayAction {
    Show,
    Exit,
}

#[cfg(windows)]
const VELOPACK_FEED_URL: &str = "https://playtracker.al1e.dev/releases/win";

fn main() -> Result<()> {
    #[cfg(windows)]
    {
        let mut velo_app = VelopackApp::build().set_auto_apply_on_startup(true);
        velo_app.run();
    }

    let snapshot = Arc::new(Mutex::new(MonitorSnapshot::default()));
    let store = Arc::new(SessionStore::new()?);
    let settings_store = SettingsStore::new(store.data_dir().to_path_buf());

    let mut status_notes = Vec::new();
    let mut initial_settings = match settings_store.load() {
        Ok(settings) => settings,
        Err(err) => {
            let msg = format!("Failed to load saved settings. Using defaults. {err}");
            eprintln!("{msg}");
            status_notes.push(msg.clone());
            AppSettings::default()
        }
    };

    #[cfg(windows)]
    {
        match startup::is_installed() {
            Ok(installed) => initial_settings.run_on_login = installed,
            Err(err) => {
                let msg = format!("Failed to query startup status: {err}");
                eprintln!("{msg}");
                status_notes.push(msg);
            }
        }
    }

    let initial_status = if status_notes.is_empty() {
        None
    } else {
        Some(status_notes.join("\n"))
    };

    let mut native_options = eframe::NativeOptions::default();
    #[cfg(windows)]
    {
        native_options.viewport = native_options.viewport.clone().with_taskbar(false);
    }
    eframe::run_native(
        "Star Citizen Playtime",
        native_options,
        Box::new(move |cc| {
            Box::new(PlaytimeApp::new(
                cc,
                Arc::clone(&store),
                Arc::clone(&snapshot),
                settings_store.clone(),
                initial_settings.clone(),
                initial_status.clone(),
            ))
        }),
    )
    .map_err(|err| anyhow!("eframe error: {err}"))
}

struct PlaytimeApp {
    store: Arc<SessionStore>,
    snapshot: Arc<Mutex<MonitorSnapshot>>,
    stop_flag: Arc<AtomicBool>,
    monitor_handle: Option<JoinHandle<()>>,
    sessions: Vec<Session>,
    analytics: Option<Analytics>,
    last_refresh: Instant,
    refresh_interval: Duration,
    settings_store: SettingsStore,
    settings: AppSettings,
    pending_settings: AppSettings,
    status_message: Option<String>,
    status_since: Option<Instant>,
    selected_tab: DashboardTab,
    style_applied: bool,
    leaderboard_client: Option<LeaderboardClient>,
    leaderboard_entries: Vec<LeaderboardEntry>,
    leaderboard_rx: Option<Receiver<LeaderboardSyncResult>>,
    leaderboard_inflight: bool,
    last_leaderboard_attempt: Option<Instant>,
    last_leaderboard_success: Option<Instant>,
    leaderboard_sync_interval: Duration,
    #[cfg(windows)]
    tray: Option<TrayController>,
    #[cfg(windows)]
    first_frame: bool,
    #[cfg(windows)]
    pending_hide: bool,
    #[cfg(windows)]
    pending_show: bool,
    #[cfg(windows)]
    pending_exit: bool,
    #[cfg(windows)]
    exit_ready: bool,
    #[cfg(windows)]
    last_tray_tick_log: Instant,
    #[cfg(windows)]
    last_update_log: Instant,
    #[cfg(windows)]
    update_manager: Option<UpdateManager>,
    #[cfg(windows)]
    update_rx: Option<Receiver<UpdateOutcome>>,
    #[cfg(windows)]
    update_inflight: bool,
    #[cfg(windows)]
    last_update_attempt: Option<Instant>,
    #[cfg(windows)]
    update_check_interval: Duration,
    #[cfg(windows)]
    window_handle: Arc<AtomicIsize>,
}

#[derive(Default)]
struct LeaderboardSyncResult {
    message: Option<String>,
    entries: Option<Vec<LeaderboardEntry>>,
    error: Option<String>,
}

#[cfg(windows)]
#[derive(Default)]
struct UpdateOutcome {
    message: Option<String>,
    error: Option<String>,
    update: Option<UpdateInfo>,
}

enum LeaderboardJob {
    SubmitAndFetch {
        username: String,
        total_minutes: f64,
    },
    FetchOnly,
}

impl PlaytimeApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        store: Arc<SessionStore>,
        snapshot: Arc<Mutex<MonitorSnapshot>>,
        settings_store: SettingsStore,
        mut initial_settings: AppSettings,
        initial_status: Option<String>,
    ) -> Self {
        initial_settings.sanitize();
        let refresh_interval = Duration::from_secs(initial_settings.refresh_seconds.max(1));
        let (status_message, status_since) = match initial_status {
            Some(message) => (Some(message), Some(Instant::now())),
            None => (None, None),
        };

        let window_handle = Arc::new(AtomicIsize::new(0));

        #[cfg(windows)]
        let update_manager = Self::initialize_update_manager();

        let mut app = Self {
            store,
            snapshot,
            stop_flag: Arc::new(AtomicBool::new(false)),
            monitor_handle: None,
            sessions: Vec::new(),
            analytics: None,
            last_refresh: Instant::now() - refresh_interval,
            refresh_interval,
            settings_store,
            settings: initial_settings.clone(),
            pending_settings: initial_settings,
            status_message,
            status_since,
            selected_tab: DashboardTab::Overview,
            style_applied: false,
            leaderboard_client: None,
            leaderboard_entries: Vec::new(),
            leaderboard_rx: None,
            leaderboard_inflight: false,
            last_leaderboard_attempt: None,
            last_leaderboard_success: None,
            leaderboard_sync_interval: Duration::from_secs(300),
            #[cfg(windows)]
            tray: TrayController::new(&cc.egui_ctx, Arc::clone(&window_handle)),
            #[cfg(windows)]
            first_frame: true,
            #[cfg(windows)]
            pending_hide: false,
            #[cfg(windows)]
            pending_show: false,
            #[cfg(windows)]
            pending_exit: false,
            #[cfg(windows)]
            exit_ready: false,
            #[cfg(windows)]
            last_tray_tick_log: Instant::now(),
            #[cfg(windows)]
            last_update_log: Instant::now(),
            #[cfg(windows)]
            update_manager,
            #[cfg(windows)]
            update_rx: None,
            #[cfg(windows)]
            update_inflight: false,
            #[cfg(windows)]
            last_update_attempt: None,
            #[cfg(windows)]
            update_check_interval: Duration::from_secs(3600),
            #[cfg(windows)]
            window_handle,
        };
        #[cfg(windows)]
        {
            if app.tray.is_some() {
                TrayController::log_event("PlaytimeApp acquired tray controller");
            } else {
                TrayController::log_event("PlaytimeApp tray controller unavailable");
            }
        }
        app.refresh_sessions();
        app.start_monitor();
        app.initialize_leaderboard_client();
        app.maybe_queue_initial_leaderboard_fetch();
        app
    }

    fn refresh_sessions(&mut self) {
        match self.store.load_sessions() {
            Ok(mut sessions) => {
                sessions.sort_by_key(|s| s.start);
                self.analytics = Some(compute_analytics(&sessions));
                self.sessions = sessions;
            }
            Err(err) => {
                eprintln!("Failed to load sessions: {err:?}");
                self.analytics = None;
                self.sessions.clear();
                self.set_status(format!("Failed to load sessions: {err}"));
            }
        }
        self.last_refresh = Instant::now();
    }

    fn start_monitor(&mut self) {
        self.stop_monitor();
        self.stop_flag = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&self.stop_flag);
        let snapshot = Arc::clone(&self.snapshot);
        let poll = self.settings.poll_seconds.max(1);
        let min_session = self.settings.min_session_minutes.max(1);
        self.monitor_handle = Some(thread::spawn(move || {
            let mut monitor =
                Monitor::new(Duration::from_secs(poll), min_session).with_status_sink(snapshot);
            if let Err(err) = monitor.run(stop) {
                eprintln!("Monitor loop error: {err:?}");
            }
        }));
    }

    #[cfg(windows)]
    fn initialize_update_manager() -> Option<UpdateManager> {
        let source = HttpSource::new(VELOPACK_FEED_URL);
        match UpdateManager::new(source, None, None) {
            Ok(manager) => Some(manager),
            Err(err) => {
                eprintln!("Velopack updates unavailable: {err:?}");
                None
            }
        }
    }

    fn stop_monitor(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.monitor_handle.take() {
            let _ = handle.join();
        }
    }

    #[cfg(windows)]
    fn maybe_check_for_updates(&mut self) {
        if self.update_manager.is_none() || self.update_inflight {
            return;
        }

        let due = match self.last_update_attempt {
            Some(last) => last.elapsed() >= self.update_check_interval,
            None => true,
        };

        if !due {
            return;
        }

        let manager = self.update_manager.as_ref().unwrap().clone();
        let (tx, rx) = mpsc::channel();
        self.update_rx = Some(rx);
        self.update_inflight = true;
        self.last_update_attempt = Some(Instant::now());

        thread::spawn(move || {
            let mut outcome = UpdateOutcome::default();
            match manager.check_for_updates() {
                Ok(UpdateCheck::UpdateAvailable(info)) => {
                    let version = info.TargetFullRelease.Version.clone();
                    match manager.download_updates(&info, None) {
                        Ok(()) => {
                            outcome.message =
                                Some(format!("Update {version} downloaded. Restarting to apply…"));
                            outcome.update = Some(info);
                        }
                        Err(err) => {
                            outcome.error =
                                Some(format!("Failed to download update {version}: {err}"));
                        }
                    }
                }
                Ok(UpdateCheck::RemoteIsEmpty) | Ok(UpdateCheck::NoUpdateAvailable) => {}
                Err(err) => {
                    outcome.error = Some(format!("Update check failed: {err}"));
                }
            }

            let _ = tx.send(outcome);
        });
    }

    #[cfg(windows)]
    fn poll_update_notifications(&mut self, ctx: &egui::Context) {
        let outcome = if let Some(rx) = &self.update_rx {
            match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(UpdateOutcome::default()),
            }
        } else {
            None
        };

        if let Some(outcome) = outcome {
            self.update_inflight = false;
            self.update_rx = None;

            if let Some(error) = outcome.error {
                self.set_status(error);
                return;
            }

            if let Some(message) = outcome.message {
                self.set_status(message.clone());
            }

            if let Some(info) = outcome.update {
                if let Some(manager) = &self.update_manager {
                    match manager.wait_exit_then_apply_updates(
                        &info,
                        true,
                        true,
                        std::iter::empty::<String>(),
                    ) {
                        Ok(()) => {
                            self.exit_ready = true;
                            self.pending_exit = true;
                            self.pending_hide = false;
                            self.pending_show = false;
                            ctx.send_viewport_cmd(ViewportCommand::Close);
                        }
                        Err(err) => {
                            self.set_status(format!("Failed to schedule update: {err}"));
                        }
                    }
                } else {
                    self.set_status(
                        "Update downloaded but update manager was unavailable.".to_string(),
                    );
                }
            }
        }
    }

    #[cfg(windows)]
    fn process_tray(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.first_frame {
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
            self.first_frame = false;
        }

        let mut tray_actions = Vec::new();
        if let Some(tray) = &self.tray {
            if self.last_tray_tick_log.elapsed() >= Duration::from_secs(1) {
                TrayController::log_event("process_tray tick");
                self.last_tray_tick_log = Instant::now();
            }
            tray.drain_actions(|action| tray_actions.push(action));
            if !tray_actions.is_empty() {
                TrayController::log_event(&format!(
                    "app collected {} tray actions",
                    tray_actions.len()
                ));
            }
        }

        for action in tray_actions {
            match action {
                TrayAction::Show => {
                    self.pending_show = true;
                    self.pending_hide = false;
                    self.set_status("Tray requested dashboard");
                    TrayController::log_event("app queued pending_show");
                }
                TrayAction::Exit => {
                    self.pending_exit = true;
                    self.set_status("Tray requested exit");
                    TrayController::log_event("app queued pending_exit");
                }
            }
        }

        if self.pending_exit {
            TrayController::log_event("app processing pending_exit");
            self.pending_exit = false;
            self.exit_ready = true;
            self.pending_show = false;
            self.pending_hide = false;
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }

        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && !self.exit_ready {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.pending_hide = true;
            TrayController::log_event("app queued pending_hide from close");
        }

        if self.pending_hide {
            TrayController::log_event("app processing pending_hide");
            self.pending_hide = false;
            self.pending_show = false;
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }

        if self.pending_show {
            TrayController::log_event("app processing pending_show");
            self.pending_show = false;
            self.exit_ready = false;
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
    }

    fn initialize_leaderboard_client(&mut self) {
        let override_endpoint = {
            let candidate = self.settings.leaderboard_endpoint.trim();
            if candidate.is_empty() {
                None
            } else {
                Some(candidate)
            }
        };
        match LeaderboardClient::auto(self.store.data_dir(), override_endpoint) {
            Ok(client) => {
                self.leaderboard_client = Some(client);
            }
            Err(err) => {
                self.leaderboard_client = None;
                eprintln!("Leaderboard client init failed: {err:?}");
                self.set_status(format!("Leaderboard unavailable: {err}"));
            }
        }
    }

    fn maybe_queue_initial_leaderboard_fetch(&mut self) {
        // Attempt an immediate refresh on startup so the UI has data ready.
        self.last_leaderboard_attempt = None;
        self.maybe_sync_leaderboard();
    }

    fn start_leaderboard_job(&mut self, job: LeaderboardJob) {
        if self.leaderboard_inflight {
            return;
        }
        let client = match &self.leaderboard_client {
            Some(client) => client.clone(),
            None => {
                self.set_status("Leaderboard service is not configured.");
                return;
            }
        };

        let (tx, rx) = mpsc::channel();
        self.leaderboard_rx = Some(rx);
        self.leaderboard_inflight = true;
        self.last_leaderboard_attempt = Some(Instant::now());

        thread::spawn(move || {
            let mut outcome = LeaderboardSyncResult::default();
            match job {
                LeaderboardJob::SubmitAndFetch {
                    username,
                    total_minutes,
                } => {
                    match client.submit_total_minutes(&username, total_minutes) {
                        Ok(()) => {
                            outcome.message = Some(format!("Leaderboard synced for {username}."));
                        }
                        Err(err) => {
                            outcome.error = Some(format!("Failed to sync leaderboard: {err}"));
                        }
                    }
                    match client.fetch_top_entries() {
                        Ok(entries) => outcome.entries = Some(entries),
                        Err(err) => {
                            let message = format!("Failed to refresh leaderboard entries: {err}");
                            outcome.error = Some(match outcome.error.take() {
                                Some(existing) => format!("{existing} | {message}"),
                                None => message,
                            });
                        }
                    }
                }
                LeaderboardJob::FetchOnly => match client.fetch_top_entries() {
                    Ok(entries) => outcome.entries = Some(entries),
                    Err(err) => {
                        outcome.error =
                            Some(format!("Failed to refresh leaderboard entries: {err}"));
                    }
                },
            }

            let _ = tx.send(outcome);
        });
    }

    fn poll_leaderboard_updates(&mut self) {
        let outcome = if let Some(rx) = &self.leaderboard_rx {
            match rx.try_recv() {
                Ok(result) => Some(Ok(result)),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(())),
            }
        } else {
            None
        };

        if let Some(outcome) = outcome {
            self.leaderboard_inflight = false;
            self.leaderboard_rx = None;

            match outcome {
                Ok(result) => {
                    if let Some(entries) = result.entries {
                        self.leaderboard_entries = entries;
                    }
                    if let Some(message) = result.message {
                        self.last_leaderboard_success = Some(Instant::now());
                        self.set_status(message);
                    }
                    if let Some(error) = result.error {
                        self.set_status(error);
                    }
                }
                Err(()) => {
                    self.set_status("Leaderboard sync interrupted.");
                }
            }
        }
    }

    fn maybe_sync_leaderboard(&mut self) {
        if self.leaderboard_inflight {
            return;
        }
        if self.leaderboard_client.is_none() {
            return;
        }

        let due = match self.last_leaderboard_attempt {
            Some(last) => last.elapsed() >= self.leaderboard_sync_interval,
            None => true,
        };

        if !due {
            return;
        }

        if self.settings.sync_leaderboard {
            let username = self.settings.leaderboard_username.trim();
            if username.is_empty() {
                if self.leaderboard_entries.is_empty() {
                    self.set_status(
                        "Add a leaderboard username in settings to appear on the leaderboard.",
                    );
                }
                self.start_leaderboard_job(LeaderboardJob::FetchOnly);
                return;
            }
            let total_minutes = self
                .analytics
                .as_ref()
                .map(|analytics| analytics.total_minutes)
                .unwrap_or(0.0);
            self.start_leaderboard_job(LeaderboardJob::SubmitAndFetch {
                username: username.to_string(),
                total_minutes,
            });
        } else {
            self.start_leaderboard_job(LeaderboardJob::FetchOnly);
        }
    }

    fn force_leaderboard_sync(&mut self) {
        self.last_leaderboard_attempt = None;
        if self.leaderboard_inflight {
            self.set_status("Leaderboard sync already in progress.");
            return;
        }
        if self.leaderboard_client.is_none() {
            self.set_status("Leaderboard service is not configured.");
            return;
        }
        if self.settings.sync_leaderboard {
            let username = self.settings.leaderboard_username.trim();
            if username.is_empty() {
                self.set_status("Enter a leaderboard username before syncing.");
                return;
            }
            let total_minutes = self
                .analytics
                .as_ref()
                .map(|analytics| analytics.total_minutes)
                .unwrap_or(0.0);
            self.start_leaderboard_job(LeaderboardJob::SubmitAndFetch {
                username: username.to_string(),
                total_minutes,
            });
        } else {
            self.start_leaderboard_job(LeaderboardJob::FetchOnly);
        }
    }

    fn apply_monitor_settings(&mut self) {
        let mut new_settings = self.pending_settings.clone();
        new_settings.sanitize();
        let changed = new_settings.poll_seconds != self.settings.poll_seconds
            || new_settings.min_session_minutes != self.settings.min_session_minutes
            || new_settings.refresh_seconds != self.settings.refresh_seconds;

        if !changed {
            self.pending_settings = self.settings.clone();
            self.set_status("Monitor settings already applied.");
            return;
        }

        self.settings = new_settings.clone();
        self.pending_settings = new_settings;
        self.refresh_interval = Duration::from_secs(self.settings.refresh_seconds.max(1));

        let save_result = self.settings_store.save(&self.settings);
        self.start_monitor();

        match save_result {
            Ok(()) => self.set_status(format!(
                "Updated monitor (poll every {}s, minimum session {}m).",
                self.settings.poll_seconds, self.settings.min_session_minutes
            )),
            Err(err) => self.set_status(format!(
                "Updated monitor but failed to save settings: {err}"
            )),
        }
    }

    fn apply_leaderboard_settings(&mut self) {
        self.pending_settings.sanitize();
        let changed = self.settings.sync_leaderboard != self.pending_settings.sync_leaderboard
            || self.settings.leaderboard_username != self.pending_settings.leaderboard_username
            || self.settings.leaderboard_endpoint != self.pending_settings.leaderboard_endpoint;

        if !changed {
            self.set_status("Leaderboard settings already applied.");
            return;
        }

        self.settings.sync_leaderboard = self.pending_settings.sync_leaderboard;
        self.settings.leaderboard_username = self.pending_settings.leaderboard_username.clone();
        self.settings.leaderboard_endpoint = self.pending_settings.leaderboard_endpoint.clone();
        self.pending_settings.sync_leaderboard = self.settings.sync_leaderboard;
        self.pending_settings.leaderboard_username = self.settings.leaderboard_username.clone();
        self.pending_settings.leaderboard_endpoint = self.settings.leaderboard_endpoint.clone();

        let save_result = self.settings_store.save(&self.settings);

        self.initialize_leaderboard_client();
        self.leaderboard_entries.clear();
        self.leaderboard_rx = None;
        self.leaderboard_inflight = false;
        self.last_leaderboard_attempt = None;

        match save_result {
            Ok(()) => {
                if self.settings.sync_leaderboard {
                    if self.settings.leaderboard_username.trim().is_empty() {
                        self.set_status(
                            "Leaderboard sync enabled. Add a username to share your playtime.",
                        );
                    } else {
                        self.set_status(format!(
                            "Leaderboard sync enabled for {}.",
                            self.settings.leaderboard_username
                        ));
                    }
                } else {
                    self.set_status("Leaderboard sync disabled.");
                }
            }
            Err(err) => {
                self.set_status(format!("Failed to save leaderboard settings: {err}"));
            }
        }

        self.maybe_sync_leaderboard();
    }

    fn ensure_style(&mut self, ctx: &egui::Context) {
        if self.style_applied {
            return;
        }
        self.style_applied = true;

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(12.0, 10.0);
        style.spacing.window_margin = Margin::symmetric(18.0, 14.0);
        style.spacing.indent = 22.0;
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        style.visuals.window_rounding = Rounding::same(12.0);
        ctx.set_style(style);

        let mut visuals = Visuals::dark();
        visuals.window_rounding = Rounding::same(12.0);
        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(26, 30, 39);
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(36, 41, 52);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(46, 51, 64);
        visuals.widgets.active.bg_fill = Color32::from_rgb(56, 61, 74);
        visuals.widgets.noninteractive.fg_stroke.color = Color32::from_rgb(210, 214, 222);
        visuals.widgets.inactive.fg_stroke.color = Color32::from_rgb(224, 228, 235);
        visuals.widgets.active.fg_stroke.color = Color32::WHITE;
        visuals.window_shadow = Shadow::NONE;
        ctx.set_visuals(visuals);
    }

    fn render_tab_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            for (tab, label) in [
                (DashboardTab::Overview, "Overview"),
                (DashboardTab::Insights, "Insights"),
            ] {
                let is_active = self.selected_tab == tab;
                let button = egui::Button::new(label)
                    .min_size(egui::vec2(120.0, 32.0))
                    .fill(if is_active {
                        Color32::from_rgb(82, 96, 122)
                    } else {
                        Color32::from_rgb(36, 41, 52)
                    })
                    .stroke(Stroke::new(
                        1.0,
                        if is_active {
                            Color32::from_rgb(130, 180, 255)
                        } else {
                            Color32::from_rgb(60, 66, 80)
                        },
                    ))
                    .rounding(Rounding::same(10.0));
                if ui.add(button).clicked() {
                    self.selected_tab = tab;
                }
            }
        });
    }

    fn render_status_banner(&self, ui: &mut egui::Ui, snapshot: &MonitorSnapshot) {
        let (status, accent, detail) = if let Some(active) = &snapshot.active_session {
            let elapsed = format_duration(active_session_minutes(active));
            (
                "Tracking",
                Color32::from_rgb(94, 201, 146),
                format!(
                    "Session started {} ({elapsed} elapsed)",
                    active.start.format("%Y-%m-%d %H:%M")
                ),
            )
        } else if let Some(last) = &snapshot.last_session {
            (
                "Idle",
                Color32::from_rgb(130, 140, 170),
                format!(
                    "Last session {} for {}",
                    last.start.format("%Y-%m-%d %H:%M"),
                    format_duration(last.duration_minutes)
                ),
            )
        } else {
            (
                "Idle",
                Color32::from_rgb(130, 140, 170),
                String::from("Waiting for Star Citizen to launch."),
            )
        };

        Frame::group(ui.style())
            .fill(Color32::from_rgb(33, 38, 49))
            .stroke(Stroke::new(1.0, accent))
            .rounding(Rounding::same(12.0))
            .inner_margin(Margin::symmetric(16.0, 12.0))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(status).color(accent).size(18.0).strong());
                    ui.add_space(4.0);
                    ui.label(detail);
                });
            });
    }

    fn render_summary_cards(&self, ui: &mut egui::Ui) {
        let cards: Vec<(&'static str, String, String, Color32)> =
            if let Some(analytics) = &self.analytics {
                vec![
                    (
                        "Total hours",
                        format!("{:.1}", analytics.total_minutes / 60.0),
                        format!("Across {} sessions", analytics.total_sessions),
                        Color32::from_rgb(86, 156, 214),
                    ),
                    (
                        "Average session",
                        format_duration(analytics.average_session_minutes),
                        format!(
                            "Median {}",
                            format_duration(analytics.median_session_minutes)
                        ),
                        Color32::from_rgb(170, 120, 255),
                    ),
                    (
                        "Last 7 days",
                        format!("{:.1}", analytics.minutes_last_7 / 60.0),
                        format!("30-day total {:.1} h", analytics.minutes_last_30 / 60.0),
                        Color32::from_rgb(255, 170, 90),
                    ),
                ]
            } else {
                vec![(
                    "No sessions yet",
                    String::from("—"),
                    String::from("Launch Star Citizen to begin tracking."),
                    Color32::from_rgb(140, 140, 160),
                )]
            };

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
            for card in cards.iter() {
                ui.scope(|ui| {
                    ui.set_min_width(170.0);
                    self.draw_stat_card(ui, card.0, &card.1, &card.2, card.3);
                });
            }
        });
    }

    fn draw_stat_card(
        &self,
        ui: &mut egui::Ui,
        title: &str,
        value: &str,
        hint: &str,
        accent: Color32,
    ) {
        Frame::group(ui.style())
            .fill(Color32::from_rgb(36, 41, 52))
            .stroke(Stroke::new(1.0, accent))
            .rounding(Rounding::same(12.0))
            .inner_margin(Margin::symmetric(14.0, 12.0))
            .show(ui, |ui| {
                ui.label(RichText::new(title).color(accent).strong());
                ui.add_space(6.0);
                ui.label(RichText::new(value).size(22.0).strong());
                ui.add_space(4.0);
                ui.label(hint);
            });
    }

    fn render_overview_tab(&mut self, ui: &mut egui::Ui, snapshot: &MonitorSnapshot) {
        self.render_status_banner(ui, snapshot);
        ui.add_space(12.0);
        self.render_summary_cards(ui);
        ui.add_space(16.0);

        ui.collapsing("Monitor & Data", |ui| {
            self.render_settings(ui);
        });

        if let Some(analytics) = &self.analytics {
            ui.add_space(16.0);
            ui.collapsing("Playtime Summary", |ui| {
                self.render_totals(ui, analytics);
                ui.add_space(8.0);
                self.render_top_days(ui, analytics);
            });
        } else {
            ui.add_space(16.0);
            ui.label("No playtime recorded yet. Launch Star Citizen to begin tracking.");
        }

        ui.add_space(16.0);
        ui.collapsing("Recent Sessions", |ui| {
            self.render_recent(ui);
        });
    }

    fn render_insights_tab(&mut self, ui: &mut egui::Ui, snapshot: &MonitorSnapshot) {
        self.render_status_banner(ui, snapshot);
        ui.add_space(12.0);

        ui.heading("Charts");
        ui.horizontal(|ui| {
            let mut daily = self.settings.show_daily_chart;
            if ui.checkbox(&mut daily, "Daily").changed() {
                self.settings.show_daily_chart = daily;
                self.pending_settings.show_daily_chart = daily;
                let message = if daily {
                    "Daily playtime chart enabled."
                } else {
                    "Daily playtime chart disabled."
                };
                self.persist_visual_setting(message);
            }
            let mut weekly = self.settings.show_weekly_chart;
            if ui.checkbox(&mut weekly, "Weekly").changed() {
                self.settings.show_weekly_chart = weekly;
                self.pending_settings.show_weekly_chart = weekly;
                let message = if weekly {
                    "Weekly playtime chart enabled."
                } else {
                    "Weekly playtime chart disabled."
                };
                self.persist_visual_setting(message);
            }
        });

        if let Some(analytics) = &self.analytics {
            ui.add_space(12.0);
            ui.columns(2, |columns| {
                columns[0].vertical(|ui| {
                    self.render_charts(ui, analytics);
                });
                columns[1].vertical(|ui| {
                    self.render_insight_stats(ui, analytics);
                    ui.add_space(16.0);
                    self.render_leaderboard(ui);
                });
            });
        } else {
            ui.label("Playtime charts will appear after the first session is recorded.");
            ui.add_space(12.0);
            self.render_leaderboard(ui);
        }
    }

    fn render_totals(&self, ui: &mut egui::Ui, analytics: &Analytics) {
        ui.label(format!(
            "Total playtime: {:.2} hours across {} sessions",
            analytics.total_minutes / 60.0,
            analytics.total_sessions
        ));
        ui.label(format!(
            "Average session: {} | Median session: {}",
            format_duration(analytics.average_session_minutes),
            format_duration(analytics.median_session_minutes)
        ));
        if let (Some(first), Some(last)) = (analytics.first_day, analytics.last_day) {
            ui.label(format!("Span: {} -> {}", fmt_day(first), fmt_day(last)));
        }
        ui.label(format!(
            "Rolling totals — 7 days: {:.2} h | 30 days: {:.2} h",
            analytics.minutes_last_7 / 60.0,
            analytics.minutes_last_30 / 60.0
        ));
    }

    fn render_recent(&self, ui: &mut egui::Ui) {
        if self.sessions.is_empty() {
            ui.label("No sessions recorded yet.");
            return;
        }
        Grid::new("recent_sessions_grid")
            .striped(true)
            .show(ui, |grid| {
                grid.label(RichText::new("Start").strong());
                grid.label(RichText::new("Duration").strong());
                grid.end_row();
                for session in self.sessions.iter().rev().take(12) {
                    grid.label(session.start.format("%Y-%m-%d %H:%M").to_string());
                    grid.label(format_duration(session.duration_minutes));
                    grid.end_row();
                }
            });
    }

    fn render_charts(&self, ui: &mut egui::Ui, analytics: &Analytics) {
        let mut any_rendered = false;

        if self.settings.show_daily_chart {
            self.render_daily_chart(ui, analytics);
            any_rendered = true;
        }

        if self.settings.show_weekly_chart {
            if any_rendered {
                ui.add_space(12.0);
            }
            self.render_weekly_chart(ui, analytics);
            any_rendered = true;
        }

        if !any_rendered {
            ui.label("Enable a chart using the toggles above to view playtime trends.");
        }
    }

    fn render_insight_stats(&self, ui: &mut egui::Ui, analytics: &Analytics) {
        ui.heading("Live statistics");

        let today = Local::now().date_naive();
        let cutoff_7 = today - ChronoDuration::days(6);
        let session_days: HashSet<NaiveDate> =
            self.sessions.iter().map(|s| s.start.date_naive()).collect();

        let sessions_last_7: usize = self
            .sessions
            .iter()
            .filter(|s| s.start.date_naive() >= cutoff_7)
            .count();
        let avg_last_7 = if sessions_last_7 > 0 {
            analytics.minutes_last_7 / sessions_last_7 as f64
        } else {
            0.0
        };

        let longest_minutes = self
            .sessions
            .iter()
            .map(|s| s.duration_minutes)
            .fold(0.0, f64::max);

        let mut streak = 0;
        let mut cursor = today;
        while session_days.contains(&cursor) {
            streak += 1;
            cursor -= ChronoDuration::days(1);
        }

        let most_recent = self.sessions.last();

        ui.label(format!(
            "Sessions this week: {sessions_last_7} ({:.2} h total)",
            analytics.minutes_last_7 / 60.0
        ));
        ui.label(format!(
            "Avg session (7 days): {}",
            format_duration(avg_last_7)
        ));
        ui.label(format!(
            "Longest session recorded: {}",
            if longest_minutes > 0.0 {
                format_duration(longest_minutes)
            } else {
                "—".to_string()
            }
        ));
        ui.label(format!("Current daily streak: {} day(s)", streak));
        if let Some(session) = most_recent {
            ui.label(format!(
                "Most recent session: {} for {}",
                session.start.format("%Y-%m-%d %H:%M"),
                format_duration(session.duration_minutes)
            ));
        }
        if let Some((day, minutes)) = analytics.top_days.first() {
            ui.label(format!(
                "Best day on record: {} ({})",
                day.format("%Y-%m-%d"),
                format_duration(*minutes)
            ));
        }
    }

    fn render_leaderboard(&self, ui: &mut egui::Ui) {
        ui.heading("Global leaderboard");
        if self.leaderboard_inflight {
            ui.label("Syncing leaderboard…");
        }

        if self.leaderboard_entries.is_empty() {
            ui.label("No leaderboard data yet.");
        } else {
            Grid::new("leaderboard_grid")
                .striped(true)
                .show(ui, |grid| {
                    grid.label(RichText::new("#").strong());
                    grid.label(RichText::new("Commander").strong());
                    grid.label(RichText::new("Hours").strong());
                    grid.end_row();
                    for (idx, entry) in self.leaderboard_entries.iter().enumerate() {
                        grid.label((idx + 1).to_string());
                        grid.label(entry.username.clone());
                        grid.label(format!("{:.2}", entry.total_minutes / 60.0));
                        grid.end_row();
                    }
                });
        }

        if let Some(success) = self.last_leaderboard_success {
            ui.label(format!(
                "Last updated {} ago.",
                format_elapsed(success.elapsed())
            ));
        }

        if !self.settings.sync_leaderboard {
            ui.label("Enable sync in settings to submit your playtime.");
        } else if self.settings.leaderboard_username.trim().is_empty() {
            ui.label("Add a username in settings to appear on the leaderboard.");
        }
    }

    fn render_daily_chart(&self, ui: &mut egui::Ui, analytics: &Analytics) {
        let mut data = analytics.recent_daily.clone();
        if data.is_empty() {
            ui.label("Daily chart not available yet.");
            return;
        }

        data.reverse();
        let labels_vec = data
            .iter()
            .map(|(day, _)| day.format("%m-%d").to_string())
            .collect::<Vec<_>>();
        let label_arc = Arc::new(labels_vec);
        let axis_labels = Arc::clone(&label_arc);
        let tooltip_labels = Arc::clone(&label_arc);
        let bar_color = Color32::from_rgb(114, 181, 244);
        let mut max_hours: f64 = 0.0;
        let bars: Vec<Bar> = data
            .iter()
            .enumerate()
            .map(|(idx, (_day, minutes))| {
                let hours = minutes / 60.0;
                max_hours = max_hours.max(hours);
                Bar::new(idx as f64, hours).width(0.8)
            })
            .collect();
        let chart_bars = bars;
        let max_hours = max_hours;

        ui.heading("Daily playtime (last 14 days)");
        Plot::new("daily_playtime_plot")
            .height(180.0)
            .allow_zoom(false)
            .allow_drag(false)
            .include_y(0.0)
            .legend(Legend::default())
            .x_axis_formatter(move |value, _range, _formatter| {
                let idx = value.value.round() as usize;
                axis_labels.get(idx).cloned().unwrap_or_default()
            })
            .label_formatter(move |series, value: &PlotPoint| {
                let idx = value.x.round() as usize;
                let date = tooltip_labels
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| String::from("?"));
                let hours = value.y;
                if series.is_empty() {
                    format!("{date}\n{hours:.2} h")
                } else {
                    format!("{series}\n{date}\n{hours:.2} h")
                }
            })
            .show(ui, move |plot_ui| {
                let upper = if max_hours <= 0.0 {
                    1.0
                } else {
                    (max_hours * 1.1).ceil()
                };
                let count = chart_bars.len();
                let x_min = -0.5;
                let x_max = if count == 0 {
                    0.5
                } else {
                    (count as f64) - 0.5
                };
                let x_max = x_max.max(x_min + 1.0);
                plot_ui.set_auto_bounds(Vec2b::new(false, false));
                plot_ui.set_plot_bounds(PlotBounds::from_min_max([x_min, 0.0], [x_max, upper]));

                let mut chart = BarChart::new(chart_bars.clone());
                chart = chart.color(bar_color).name("Hours per day");
                plot_ui.bar_chart(chart);
            });
    }

    fn render_weekly_chart(&self, ui: &mut egui::Ui, analytics: &Analytics) {
        let mut data = analytics.recent_weekly.clone();
        if data.is_empty() {
            ui.label("Weekly chart not available yet.");
            return;
        }

        data.reverse();
        let labels_vec = data
            .iter()
            .map(|((year, week), _)| format!("{year}-W{week:02}"))
            .collect::<Vec<_>>();
        let label_arc = Arc::new(labels_vec);
        let axis_labels = Arc::clone(&label_arc);
        let tooltip_labels = Arc::clone(&label_arc);
        let bar_color = Color32::from_rgb(255, 196, 125);
        let mut max_hours: f64 = 0.0;
        let bars: Vec<Bar> = data
            .iter()
            .enumerate()
            .map(|(idx, (_week, minutes))| {
                let hours = minutes / 60.0;
                max_hours = max_hours.max(hours);
                Bar::new(idx as f64, hours).width(0.8)
            })
            .collect();
        let chart_bars = bars;
        let max_hours = max_hours;

        ui.heading("Weekly playtime (last 8 weeks)");
        Plot::new("weekly_playtime_plot")
            .height(180.0)
            .allow_zoom(false)
            .allow_drag(false)
            .include_y(0.0)
            .legend(Legend::default())
            .x_axis_formatter(move |value, _range, _formatter| {
                let idx = value.value.round() as usize;
                axis_labels.get(idx).cloned().unwrap_or_default()
            })
            .label_formatter(move |series, value: &PlotPoint| {
                let idx = value.x.round() as usize;
                let label = tooltip_labels
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| String::from("?"));
                let hours = value.y;
                if series.is_empty() {
                    format!("{label}\n{hours:.2} h")
                } else {
                    format!("{series}\n{label}\n{hours:.2} h")
                }
            })
            .show(ui, move |plot_ui| {
                let upper = if max_hours <= 0.0 {
                    1.0
                } else {
                    (max_hours * 1.1).ceil()
                };
                let count = chart_bars.len();
                let x_min = -0.5;
                let x_max = if count == 0 {
                    0.5
                } else {
                    (count as f64) - 0.5
                };
                let x_max = x_max.max(x_min + 1.0);
                plot_ui.set_auto_bounds(Vec2b::new(false, false));
                plot_ui.set_plot_bounds(PlotBounds::from_min_max([x_min, 0.0], [x_max, upper]));

                let mut chart = BarChart::new(chart_bars.clone());
                chart = chart.color(bar_color).name("Hours per week");
                plot_ui.bar_chart(chart);
            });
    }

    fn render_top_days(&self, ui: &mut egui::Ui, analytics: &Analytics) {
        if analytics.top_days.is_empty() {
            ui.label("No top days yet.");
            return;
        }
        ui.heading("Top Days");
        Grid::new("top_days_grid").striped(true).show(ui, |grid| {
            grid.label(RichText::new("#").strong());
            grid.label(RichText::new("Date").strong());
            grid.label(RichText::new("Playtime").strong());
            grid.end_row();
            for (idx, (day, minutes)) in analytics.top_days.iter().enumerate() {
                grid.label((idx + 1).to_string());
                grid.label(day.format("%Y-%m-%d").to_string());
                grid.label(format_duration(*minutes));
                grid.end_row();
            }
        });
    }

    fn render_settings(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Monitor Settings");
            ui.horizontal(|ui| {
                ui.label("Poll interval (seconds)");
                ui.add(
                    egui::DragValue::new(&mut self.pending_settings.poll_seconds)
                        .clamp_range(5..=600)
                        .speed(1.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Minimum session (minutes)");
                ui.add(
                    egui::DragValue::new(&mut self.pending_settings.min_session_minutes)
                        .clamp_range(1..=240)
                        .speed(1.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Dashboard refresh (seconds)");
                ui.add(
                    egui::DragValue::new(&mut self.pending_settings.refresh_seconds)
                        .clamp_range(1..=30)
                        .speed(0.2),
                );
            });
            if ui.button("Apply monitor settings").clicked() {
                self.apply_monitor_settings();
            }

            ui.separator();
            if ui.button("Refresh now").clicked() {
                self.refresh_sessions();
                self.set_status("Sessions refreshed.");
            }

            #[cfg(windows)]
            {
                let mut run_on_login = self.settings.run_on_login;
                if ui
                    .checkbox(&mut run_on_login, "Start dashboard with Windows")
                    .changed()
                {
                    if run_on_login {
                        self.enable_startup();
                    } else {
                        self.disable_startup();
                    }
                }
            }

            #[cfg(not(windows))]
            {
                ui.label("Startup registration is only available on Windows.");
            }

            ui.separator();
            ui.heading("Leaderboard Sync");
            let mut sync_enabled = self.pending_settings.sync_leaderboard;
            if ui
                .checkbox(&mut sync_enabled, "Enable leaderboard sync")
                .changed()
            {
                self.pending_settings.sync_leaderboard = sync_enabled;
            }

            ui.horizontal(|ui| {
                ui.label("Leaderboard username");
                ui.add(
                    egui::TextEdit::singleline(&mut self.pending_settings.leaderboard_username)
                        .hint_text("Commander name")
                        .desired_width(200.0),
                );
            });

            ui.horizontal(|ui| {
                ui.label("Leaderboard endpoint");
                ui.add(
                    egui::TextEdit::singleline(&mut self.pending_settings.leaderboard_endpoint)
                        .hint_text("https://playtracker.example.com")
                        .desired_width(280.0),
                );
            });
            ui.label(
                egui::RichText::new(
                    "Leave blank to use the baked-in endpoint or fall back to local storage.",
                )
                .small(),
            );

            if ui.button("Apply leaderboard settings").clicked() {
                self.apply_leaderboard_settings();
            }

            match &self.leaderboard_client {
                Some(LeaderboardClient::Remote { endpoint, .. }) => {
                    ui.label(
                        egui::RichText::new(format!(
                            "Active remote endpoint: {}",
                            endpoint.as_ref()
                        ))
                        .small(),
                    );
                }
                Some(LeaderboardClient::Local { .. }) => {
                    ui.label(egui::RichText::new("Using local leaderboard storage.").small());
                }
                None => {
                    ui.label(egui::RichText::new("Leaderboard client unavailable.").small());
                }
            }

            if self.settings.sync_leaderboard {
                if self.leaderboard_inflight {
                    ui.label("Sync in progress…");
                } else if let Some(success) = self.last_leaderboard_success {
                    ui.label(format!(
                        "Last successful sync {} ago.",
                        format_elapsed(success.elapsed())
                    ));
                } else {
                    ui.label("Leaderboard sync is enabled.");
                }

                if ui
                    .add_enabled(
                        !self.leaderboard_inflight,
                        egui::Button::new("Sync leaderboard now"),
                    )
                    .clicked()
                {
                    self.force_leaderboard_sync();
                }
            } else {
                ui.label("Leaderboard sync is disabled.");
            }

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Export CSV...").clicked() {
                    self.export_sessions();
                }
                if ui.button("Clear active session marker").clicked() {
                    self.reset_active();
                }
                if ui.button("Open data folder").clicked() {
                    self.open_data_dir();
                }
            });

            ui.label(format!("Data folder: {}", self.store.data_dir().display()));
        });
    }

    fn export_sessions(&mut self) {
        if let Some(path) = FileDialog::new()
            .set_file_name("star_citizen_playtime.csv")
            .save_file()
        {
            match self.store.export_csv(&path, &self.sessions) {
                Ok((count, actual)) => {
                    self.set_status(format!("Exported {count} sessions to {}", actual.display()));
                }
                Err(err) => {
                    self.set_status(format!("Failed to export sessions: {err}"));
                }
            }
        }
    }

    fn reset_active(&mut self) {
        match self.store.clear_active() {
            Ok(()) => {
                self.refresh_sessions();
                self.set_status("Cleared active session marker.");
            }
            Err(err) => self.set_status(format!("Failed to clear active session: {err}")),
        }
    }

    fn persist_visual_setting<S: Into<String>>(&mut self, message: S) {
        match self.settings_store.save(&self.settings) {
            Ok(()) => self.set_status(message),
            Err(err) => self.set_status(format!("Failed to save settings: {err}")),
        }
    }

    #[cfg(windows)]
    fn open_data_dir(&mut self) {
        let path = self.store.data_dir().to_path_buf();
        match Command::new("explorer").arg(&path).status() {
            Ok(_) => self.set_status("Opened data folder in Explorer."),
            Err(err) => self.set_status(format!("Failed to open data folder: {err}")),
        }
    }

    #[cfg(not(windows))]
    fn open_data_dir(&mut self) {
        self.set_status(format!("Data folder: {}", self.store.data_dir().display()));
    }

    #[cfg(windows)]
    fn enable_startup(&mut self) {
        let exe = match env::current_exe() {
            Ok(path) => path,
            Err(err) => {
                self.set_status(format!("Failed to resolve executable path: {err}"));
                return;
            }
        };
        match startup::install(&exe, "") {
            Ok(()) => {
                self.settings.run_on_login = true;
                self.pending_settings.run_on_login = true;
                match self.settings_store.save(&self.settings) {
                    Ok(()) => self.set_status("Dashboard will start with Windows."),
                    Err(err) => self.set_status(format!(
                        "Enabled startup but failed to save settings: {err}"
                    )),
                }
            }
            Err(err) => self.set_status(format!("Failed to enable startup: {err}")),
        }
    }

    #[cfg(windows)]
    fn disable_startup(&mut self) {
        match startup::uninstall() {
            Ok(()) => {
                self.settings.run_on_login = false;
                self.pending_settings.run_on_login = false;
                match self.settings_store.save(&self.settings) {
                    Ok(()) => self.set_status("Removed Windows startup entry."),
                    Err(err) => self.set_status(format!(
                        "Disabled startup but failed to save settings: {err}"
                    )),
                }
            }
            Err(err) => self.set_status(format!("Failed to disable startup: {err}")),
        }
    }

    #[cfg(not(windows))]
    fn enable_startup(&mut self) {
        self.set_status("Startup registration is only available on Windows.");
    }

    #[cfg(not(windows))]
    fn disable_startup(&mut self) {
        self.set_status("Startup registration is only available on Windows.");
    }

    fn set_status<S: Into<String>>(&mut self, message: S) {
        self.status_message = Some(message.into());
        self.status_since = Some(Instant::now());
    }

    fn maybe_clear_status(&mut self) {
        if let Some(since) = self.status_since {
            if since.elapsed() > Duration::from_secs(10) {
                self.status_message = None;
                self.status_since = None;
            }
        }
    }
}

impl eframe::App for PlaytimeApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        #[cfg(windows)]
        if self.window_handle.load(Ordering::SeqCst) == 0 {
            if let Ok(window_handle) = frame.window_handle() {
                match window_handle.as_raw() {
                    RawWindowHandle::Win32(handle) => {
                        let hwnd = handle.hwnd.get() as isize;
                        if hwnd != 0 {
                            self.window_handle.store(hwnd, Ordering::SeqCst);
                            TrayController::log_event(&format!(
                                "captured window handle: {:?}",
                                hwnd as usize
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }

        #[cfg(windows)]
        if self.last_update_log.elapsed() >= Duration::from_secs(1) {
            TrayController::log_event("update tick");
            self.last_update_log = Instant::now();
        }

        self.maybe_clear_status();
        self.ensure_style(ctx);

        #[cfg(windows)]
        self.process_tray(ctx, frame);

        #[cfg(windows)]
        self.poll_update_notifications(ctx);

        #[cfg(windows)]
        self.maybe_check_for_updates();

        if self.last_refresh.elapsed() >= self.refresh_interval {
            self.refresh_sessions();
        }

        self.poll_leaderboard_updates();
        self.maybe_sync_leaderboard();

        let snapshot = self
            .snapshot
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Star Citizen Playtime");
            ui.separator();
            self.render_tab_bar(ui);
            ui.separator();
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match self.selected_tab {
                    DashboardTab::Overview => self.render_overview_tab(ui, &snapshot),
                    DashboardTab::Insights => self.render_insights_tab(ui, &snapshot),
                });

            if let Some(message) = &self.status_message {
                ui.separator();
                ui.label(message);
            }
        });

        ctx.request_repaint();
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

impl Drop for PlaytimeApp {
    fn drop(&mut self) {
        self.stop_monitor();
    }
}

fn fmt_day(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

fn format_elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{}s", seconds)
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        if minutes == 0 {
            format!("{}h", hours)
        } else {
            format!("{}h {}m", hours, minutes)
        }
    } else {
        let days = seconds / 86_400;
        let hours = (seconds % 86_400) / 3600;
        if hours == 0 {
            format!("{}d", days)
        } else {
            format!("{}d {}h", days, hours)
        }
    }
}
