// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};
use std::process::Child;

use crate::logger;
use crate::paths::Paths;
use crate::state::Settings;

/// Launches the game and returns a handle to wait on (None when the process
/// detached). The returned `Option<Child>` is None for Windows Store-style launches.
pub fn launch(
    paths: &'static Paths,
    is_studio: bool,
    version_dir: &Path,
    launch_args: &str,
) -> Result<Option<Child>, String> {
    if cfg!(windows) {
        let exe = version_dir.join(crate::packages::executable_name(is_studio));
        logger::info(&format!("Starting Roblox: {}", exe.display()));
        let mut command = std::process::Command::new(&exe);
        command.current_dir(version_dir);
        if !launch_args.is_empty() {
            command.arg(launch_args);
        }
        match command.spawn() {
            Ok(child) => Ok(Some(child)),
            Err(e) => {
                // a broken executable gets removed so the next run reinstalls cleanly
                let _ = std::fs::remove_file(&exe);
                Err(format!("Failed to start Roblox: {e}"))
            }
        }
    } else if is_studio {
        launch_studio_via_wine(paths, version_dir, launch_args)
    } else {
        crate::sober::launch(launch_args).map(Some)
    }
}

/// Linux Studio runs the Windows binaries through Wine with a dedicated prefix.
/// Ruststrap relies on a system Wine install instead of shipping a Wine build.
fn launch_studio_via_wine(
    paths: &'static Paths,
    version_dir: &Path,
    launch_args: &str,
) -> Result<Option<Child>, String> {
    let wine = which_wine().ok_or_else(|| "Wine is required to run Roblox Studio on Linux; install wine and try again".to_string())?;

    let prefix = paths.data_root.join("Wine").join("prefixes").join("studio");
    if !prefix.join("drive_c/windows").exists() {
        logger::info("Initializing Wine prefix for Studio");
        let status = std::process::Command::new(&wine)
            .arg("-u")
            .env("WINEPREFIX", &prefix)
            .env("WINEDEBUG", "-all")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("cannot run wineboot: {e}"))?;
        if !status.success() {
            logger::warn("wineboot exited non-zero, continuing anyway");
        }
    }

    let exe = version_dir.join(crate::packages::executable_name(true));
    if !exe.exists() {
        return Err(format!("Studio executable not found at {}", exe.display()));
    }

    logger::info(&format!("Starting Studio through Wine: {}", exe.display()));
    let mut command = std::process::Command::new(wine);
    command
        .arg(&exe)
        .env("WINEPREFIX", &prefix)
        .env("WINEDEBUG", "-all")
        .current_dir(version_dir);
    if !launch_args.is_empty() {
        command.arg(launch_args);
    }
    command
        .spawn()
        .map(Some)
        .map_err(|e| format!("Failed to start Roblox Studio: {e}"))
}

fn which_wine() -> Option<PathBuf> {
    ["wine", "wine64"].iter().find_map(|name| {
        std::process::Command::new("sh")
            .args(["-c", &format!("command -v {name} 2>/dev/null")])
            .output()
            .ok()
            .filter(|out| out.status.success() && !out.stdout.is_empty())
            .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string()))
    })
}

/// Applies theme + background-app settings to Roblox's client storage.
pub fn apply_app_storage(paths: &Paths, settings: &Settings) {
    crate::appstorage::apply(paths, settings);
}
