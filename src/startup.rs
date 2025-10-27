#[cfg(windows)]
use anyhow::{Context, Result};
#[cfg(windows)]
use std::path::Path;
#[cfg(windows)]
use winreg::enums::{HKEY_CURRENT_USER, KEY_ALL_ACCESS, KEY_READ};
#[cfg(windows)]
use winreg::RegKey;

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const VALUE_NAME: &str = "StarCitizenPlaytime";

#[cfg(windows)]
pub fn install(executable: &Path, args: &str) -> Result<()> {
    if !executable.exists() {
        anyhow::bail!("Executable {} does not exist", executable.display());
    }
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey_with_flags(RUN_KEY, KEY_ALL_ACCESS)
        .context("Failed to open HKCU Run registry key")?;
    let mut command = format!("\"{}\"", executable.display());
    if !args.is_empty() {
        command.push(' ');
        command.push_str(args);
    }
    key.set_value(VALUE_NAME, &command)
        .context("Failed to set Run entry")?;
    println!("Registered auto-start entry at login.");
    Ok(())
}

#[cfg(windows)]
pub fn uninstall() -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey_with_flags(RUN_KEY, KEY_ALL_ACCESS)
        .context("Failed to open HKCU Run registry key")?;
    match key.delete_value(VALUE_NAME) {
        Ok(()) => println!("Removed auto-start entry."),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("Auto-start entry was not present.");
        }
        Err(e) => return Err(e).context("Failed to delete Run entry"),
    }
    Ok(())
}

#[cfg(windows)]
pub fn is_installed() -> Result<bool> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey_with_flags(RUN_KEY, KEY_READ)
        .context("Failed to open HKCU Run registry key")?;
    match key.get_value::<String, _>(VALUE_NAME) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).context("Failed to read Run entry"),
    }
}

#[cfg(not(windows))]
pub fn install(_executable: &std::path::Path, _args: &str) -> anyhow::Result<()> {
    anyhow::bail!("Startup registration is only available on Windows.")
}

#[cfg(not(windows))]
pub fn uninstall() -> anyhow::Result<()> {
    anyhow::bail!("Startup registration is only available on Windows.")
}

#[cfg(not(windows))]
pub fn is_installed() -> anyhow::Result<bool> {
    Ok(false)
}
