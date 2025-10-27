# Star Citizen Playtime Tracker (Rust)

A native Rust utility that tracks Star Citizen playtime automatically. The executable runs quietly in the background, detects when `StarCitizen.exe` (and related binaries) start or stop, and persists sessions to `%APPDATA%/StarCitizenPlaytime`. Built as a single binary for minimal system footprint.

## Features

- Low-overhead process polling written in Rust (no Python or runtime dependencies).
- Automatic session handling: resumes partial sessions, ignores blips shorter than 3 minutes, and writes durable JSON history.
- Built-in analytics report (`report` sub-command) and CSV export.
- Optional Windows auto-start registration via the `Run` registry key.

## Building

1. Install the Rust toolchain (https://rustup.rs/) if you have not already.
2. From the project root, build a release executable:
   ```powershell
   cargo build --release
   ```
3. The binary will be at `target\release\star_citizen_playtime.exe`.

## Usage

Run the monitor (default command when no sub-command is provided):
```powershell
star_citizen_playtime.exe            # uses default 15s poll, 3 min minimum
# or specify options
star_citizen_playtime.exe run --poll-seconds 10 --min-session-minutes 2
```

Generate a quick analytics summary:
```powershell
star_citizen_playtime.exe report
```

Export sessions to CSV:
```powershell
star_citizen_playtime.exe export-csv playtime.csv
```

## Configure Auto-start (Windows)

Register the tracker to launch at login (defaults to the current executable path):
```powershell
star_citizen_playtime.exe install-startup
```

To customize the executable path or pass arguments:
```powershell
star_citizen_playtime.exe install-startup --exe "C:\Path\To\star_citizen_playtime.exe" --args "run --poll-seconds 10"
```

Remove the auto-start entry at any time:
```powershell
star_citizen_playtime.exe uninstall-startup
```

## Data Storage

- Logs live in `%APPDATA%/StarCitizenPlaytime/sessions.json`.
- In-flight sessions are stored in `%APPDATA%/StarCitizenPlaytime/active_session.json` to survive reboots.
- CSV exports are written wherever you point the `export-csv` command.

## Distributing a Single EXE

For a portable executable you can share, copy `target\release\star_citizen_playtime.exe`. The binary has no external dependencies and can be registered for auto-start on any Windows machine with a single command (as shown above).
