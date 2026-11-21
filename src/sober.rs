// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

use crate::logger;
use crate::paths::{home_dir, Paths, SOBER_FLATPAK_ID};

/// Sober's data directory lives inside Ruststrap's versions folder; the flatpak
/// gets redirected there via a symlink so both share one storage location.
pub fn data_dir(paths: &Paths) -> PathBuf {
    paths.roblox.clone()
}

pub fn config_file(paths: &Paths) -> PathBuf {
    paths.roblox.join("config/sober/config.json")
}

fn flatpak_ok(args: &[&str]) -> Result<bool, String> {
    match Command::new("flatpak").args(args).output() {
        Ok(output) => Ok(output.status.success()),
        Err(e) => Err(format!("cannot run flatpak: {e}")),
    }
}

pub fn is_installed() -> bool {
    flatpak_ok(&["info", SOBER_FLATPAK_ID]).unwrap_or(false)
}

/// Verifies flatpak works and Sober is installed, installing it from Flathub if missing.
pub fn ensure_installed() -> Result<(), String> {
    logger::info("Checking Flatpak installation");
    match flatpak_ok(&["--version"]) {
        Ok(true) => {}
        Ok(false) => return Err("Flatpak is broken or misconfigured".to_string()),
        Err(_) => return Err("Flatpak is required on Linux; install Flatpak and try again".to_string()),
    }

    if is_installed() {
        logger::info("Sober is already installed");
        return Ok(());
    }

    logger::info("Installing Sober from Flathub (may take a while)");
    let status = Command::new("flatpak")
        .args(["install", "--noninteractive", "flathub", SOBER_FLATPAK_ID])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("cannot run flatpak: {e}"))?;
    if !status.success() {
        return Err(format!("flatpak install {SOBER_FLATPAK_ID} failed"));
    }
    logger::info("Sober installed");
    Ok(())
}

/// Updates an existing Sober flatpak to the latest Flathub build.
pub fn update() -> Result<(), String> {
    logger::info("Updating Sober via flatpak (may take a while)");
    let status = Command::new("flatpak")
        .args(["update", "--noninteractive", SOBER_FLATPAK_ID])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("cannot run flatpak: {e}"))?;
    if !status.success() {
        return Err(format!("flatpak update {SOBER_FLATPAK_ID} failed"));
    }
    logger::info("Sober updated");
    Ok(())
}

/// Redirects the flatpak's data directory into Ruststrap's storage.
pub fn setup_symlink(paths: &Paths) {
    let flatpak_data = home_dir()
        .join(".var/app")
        .join(SOBER_FLATPAK_ID);
    let target = data_dir(paths);

    if let Ok(resolved) = std::fs::read_link(&flatpak_data) {
        if resolved == target {
            logger::info("Sober symlink already in place");
            return;
        }
        let _ = std::fs::remove_file(&flatpak_data);
    }

    let _ = std::fs::create_dir_all(&target);

    if flatpak_data.exists() {
        logger::info("Moving existing Sober data into Ruststrap storage");
        if let Err(e) = copy_tree(&flatpak_data, &target) {
            logger::error(&format!("Failed to move Sober data: {e}; skipping symlink setup"));
            return;
        }
        let _ = std::fs::remove_dir_all(&flatpak_data);
    }
    if let Some(parent) = flatpak_data.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    #[cfg(unix)]
    make_symlink(&target, &flatpak_data);
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) {
    match std::os::unix::fs::symlink(target, link) {
        Ok(()) => logger::info(&format!(
            "Created symlink: {} -> {}",
            link.display(),
            target.display()
        )),
        Err(e) => logger::error(&format!("Failed to create Sober symlink: {e}")),
    }
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Loads Sober's config.json as a JSON object (empty when missing or unreadable).
/// This is the single source of truth for fast flags and Sober's own options on Linux.
pub fn load_config(paths: &Paths) -> Map<String, Value> {
    let path = config_file(paths);
    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<Value>(&contents) {
            Ok(value) => value.as_object().cloned().unwrap_or_default(),
            Err(e) => {
                logger::error(&format!(
                    "{} is not valid JSON ({}); using defaults",
                    path.display(),
                    e
                ));
                Map::new()
            }
        },
        Err(_) => Map::new(),
    }
}

/// Writes Sober's config.json, preserving any keys the caller left untouched.
pub fn save_config(paths: &Paths, config: &Map<String, Value>) -> Result<(), String> {
    let path = config_file(paths);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let contents = serde_json::to_string_pretty(config)
        .map_err(|e| format!("serialize Sober config: {e}"))?;
    std::fs::write(&path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Reads a Sober config key, accepting typed or string-encoded booleans.
pub fn config_bool(config: &Map<String, Value>, key: &str) -> Option<bool> {
    match config.get(key) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) => Some(s.eq_ignore_ascii_case("true")),
        _ => None,
    }
}

pub fn kill() {
    logger::info("Killing any running Sober instance");
    let _ = Command::new("flatpak")
        .args(["kill", SOBER_FLATPAK_ID])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Launches Sober, translating the launch URI into its supported format.
pub fn launch(launch_uri: &str) -> Result<std::process::Child, String> {
    let uri = if launch_uri.is_empty() {
        "roblox://navigation/home".to_string()
    } else if launch_uri.starts_with("roblox://") {
        launch_uri.to_string()
    } else {
        let data = crate::gamejoin::get_join_data(launch_uri);
        crate::gamejoin::build_sober_uri(&data).ok_or_else(|| {
            format!("cannot launch Sober: could not extract a joinable experience from {launch_uri}")
        })?
    };

    logger::info(&format!("Launching Sober via flatpak: {uri}"));
    Command::new("flatpak")
        .args(["run", SOBER_FLATPAK_ID, &uri])
        .spawn()
        .map_err(|e| format!("failed to start Sober: {e}"))
}
