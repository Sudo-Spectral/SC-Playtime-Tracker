use std::{path::PathBuf, sync::Arc, sync::atomic::AtomicBool, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use star_citizen_playtime::monitor::Monitor;
use star_citizen_playtime::startup;
use star_citizen_playtime::storage::{SessionStore, compute_analytics, format_duration};

#[derive(Parser, Debug)]
#[command(author, version, about = "Star Citizen playtime tracker", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the background detector loop (default command)
    Run {
        /// Polling interval in seconds
        #[arg(long, default_value_t = 15)]
        poll_seconds: u64,
        /// Minimum session length in minutes before logging
        #[arg(long, default_value_t = 3)]
        min_session_minutes: u64,
    },
    /// Print a quick analytics summary to stdout
    Report,
    /// Export session history to CSV
    ExportCsv {
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
    /// Register this executable to run on Windows login
    InstallStartup {
        #[arg(long, value_name = "EXE", default_value = "")]
        exe: String,
        #[arg(long, value_name = "ARGS", default_value = "")]
        args: String,
    },
    /// Remove the Windows startup registration
    UninstallStartup,
    /// Clear any in-progress session marker
    ResetActive,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run {
        poll_seconds: 15,
        min_session_minutes: 3,
    }) {
        Command::Run {
            poll_seconds,
            min_session_minutes,
        } => run_monitor(poll_seconds, min_session_minutes),
        Command::Report => run_report(),
        Command::ExportCsv { path } => export_csv(path),
        Command::InstallStartup { exe, args } => install_startup(exe, args),
        Command::UninstallStartup => uninstall_startup(),
        Command::ResetActive => reset_active(),
    }
}

fn run_monitor(poll_seconds: u64, min_session_minutes: u64) -> Result<()> {
    let stop_flag = Arc::new(AtomicBool::new(false));

    let mut monitor = Monitor::new(Duration::from_secs(poll_seconds), min_session_minutes);
    monitor.run(stop_flag)
}

fn run_report() -> Result<()> {
    let store = SessionStore::new()?;
    let sessions = store.load_sessions()?;
    if sessions.is_empty() {
        println!("No Sessions recorded yet.");
        return Ok(());
    }
    let analytics = compute_analytics(&sessions);
    println!(
        "Playtime Summary\n\nTotal playtime: {:.2} hours across {} sessions",
        analytics.total_minutes / 60.0,
        analytics.total_sessions
    );
    println!(
        "Average session: {} | Median session: {}",
        format_duration(analytics.average_session_minutes),
        format_duration(analytics.median_session_minutes)
    );
    if let (Some(first), Some(last)) = (analytics.first_day, analytics.last_day) {
        println!("Span: {} ➜ {}", first, last);
    }
    println!(
        "Rolling totals — 7 days: {:.2} h | 30 days: {:.2} h",
        analytics.minutes_last_7 / 60.0,
        analytics.minutes_last_30 / 60.0
    );
    println!("\nTop play days:");
    for (idx, entry) in analytics.top_days.iter().enumerate() {
        println!(" {}. {} — {}", idx + 1, entry.0, format_duration(entry.1));
    }
    println!("\nRecent sessions:");
    for session in &analytics.recent_sessions {
        println!(
            " - {} | {}",
            session.start.format("%Y-%m-%d %H:%M"),
            format_duration(session.duration_minutes)
        );
    }
    Ok(())
}

fn export_csv(path: PathBuf) -> Result<()> {
    let store = SessionStore::new()?;
    let sessions = store.load_sessions()?;
    let (written, actual_path) = store.export_csv(&path, &sessions)?;
    println!("Exported {written} sessions to {}", actual_path.display());
    Ok(())
}

fn install_startup(exe: String, args: String) -> Result<()> {
    #[cfg(windows)]
    {
        use startup::install;
        use std::env;

        let exe_path = if exe.trim().is_empty() {
            env::current_exe()?
        } else {
            PathBuf::from(exe)
        };
        install(&exe_path, args.trim())?
    }
    #[cfg(not(windows))]
    {
        println!("Startup registration is only available on Windows.");
    }
    Ok(())
}

fn uninstall_startup() -> Result<()> {
    #[cfg(windows)]
    {
        startup::uninstall()?;
    }
    #[cfg(not(windows))]
    {
        println!("Startup registration is only available on Windows.");
    }
    Ok(())
}

fn reset_active() -> Result<()> {
    let store = SessionStore::new()?;
    store.clear_active()?;
    println!("Cleared active session marker.");
    Ok(())
}
