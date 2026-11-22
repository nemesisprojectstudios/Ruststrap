// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

//! Windows player: manages the stock Roblox install the official installer
//! maintains, instead of shipping Roblox packages. Sober plays the same role
//! on Linux: Ruststrap only keeps the registry entries pointed at itself,
//! applies modifications, and launches.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::SystemTime;

use crate::logger;
use crate::paths::Paths;
use crate::progress;

const WINGET_ID: &str = "Roblox.Roblox";

fn without_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW)
    }
    #[cfg(not(windows))]
    {
        cmd
    }
}

/// The root the official Roblox installer keeps its client versions in
/// (%LOCALAPPDATA%\Roblox\Versions).
pub fn versions_root(paths: &Paths) -> PathBuf {
    paths.roblox.join("Versions")
}

fn read_versions(paths: &Paths) -> Vec<PathBuf> {
    let root = versions_root(paths);
    match std::fs::read_dir(&root) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn version_key(dirs: &[PathBuf], guid: &str) -> Option<PathBuf> {
    dirs.iter()
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().eq_ignore_ascii_case(guid))
                .unwrap_or(false)
        })
        .cloned()
}

/// The version directory the stock install considers current: the matching
/// `version-<guid>` directory when known, else the most recently touched one.
pub fn installed_dir(paths: &Paths, guid: Option<&str>) -> Option<PathBuf> {
    let dirs = read_versions(paths);
    if let Some(guid) = guid {
        if let Some(dir) = version_key(&dirs, guid) {
            return Some(dir);
        }
    }
    dirs.into_iter()
        .filter(|p| p.join("RobloxPlayerBeta.exe").is_file())
        .max_by_key(|p| {
            p.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH)
        })
}

pub fn winget_available() -> bool {
    let mut probe = Command::new("winget");
    probe.arg("--version");
    matches!(
        without_window(&mut probe)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
        Ok(status) if status.success()
    )
}

fn winget(action: &str) -> Result<(), String> {
    progress::set("Running winget");
    let mut cmd = Command::new("winget");
    let cmd = without_window(cmd.args([
        action,
        "--id",
        WINGET_ID,
        "--exact",
        "--silent",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--disable-interactivity",
    ]));
    logger::info(&format!("Running winget {action} {WINGET_ID}"));
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run winget: {e}"))?;
    if !status.success() {
        return Err(format!("winget {action} failed (exit code {status:?})"));
    }
    Ok(())
}

/// Ensures a stock Roblox player install exists and, when the expected
/// version is known, matches it. `force` upgrades even when it matches.
pub fn ensure_stock_player(
    paths: &Paths,
    expected_guid: Option<&str>,
    force: bool,
) -> Result<PathBuf, String> {
    let root = versions_root(paths);
    let have = installed_dir(paths, expected_guid);
    let up_to_date = match (expected_guid, have.as_ref()) {
        (Some(guid), Some(dir)) => dir
            .file_name()
            .map(|n| n.to_string_lossy().eq_ignore_ascii_case(guid))
            .unwrap_or(false),
        _ => false,
    };
    if let Some(dir) = &have {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        if up_to_date && !force {
            logger::info(&format!("Stock Roblox client is up to date: {name}"));
            return Ok(dir.clone());
        }
        logger::info(&format!(
            "Found stock Roblox client: {name} (expected {})",
            expected_guid.unwrap_or("<unknown>")
        ));
    } else {
        logger::info(&format!(
            "No Roblox client found under {}",
            root.display()
        ));
    }

    if !winget_available() {
        if let Some(dir) = &have {
            logger::warn("winget is unavailable; keeping the existing stock client and continuing");
            return Ok(dir.clone());
        }
        return Err(
            "Roblox is not installed and winget is unavailable; install Roblox from the official launcher (https://www.roblox.com/download) and relaunch".to_string(),
        );
    }

    let fresh = have.is_none();
    if !fresh {
        crate::processes::kill_images(&["RobloxPlayerBeta.exe"]);
    }
    let mut failed = false;
    if fresh {
        if let Err(e) = winget("install") {
            // winget may already know the package even without a usable local
            // client (wiped version directories); try to repair it
            logger::warn(&format!("winget install did not complete ({e}), trying upgrade"));
            failed = winget("upgrade").is_err();
            if failed {
                return Err(e);
            }
        }
    } else {
        // upgrade returns success even when nothing newer is deployed
        if let Err(e) = winget("upgrade") {
            logger::warn(&format!("winget upgrade did not complete: {e}"));
        }
    }

    if failed {
        return Err("winget could not install Roblox".to_string());
    }

    let now = installed_dir(paths, expected_guid);
    match now {
        Some(dir) => {
            if fresh {
                // the stock installer auto-launches the client; Ruststrap
                // spawns its own copy, so avoid running two instances
                crate::processes::kill_images(&["RobloxPlayerBeta.exe"]);
            }
            logger::info(&format!(
                "Using stock Roblox install: {}",
                dir.display()
            ));
            Ok(dir)
        }
        None if have.is_some() => {
            let dir = have.unwrap();
            logger::warn(&format!(
                "Winget finished without a matching client appearing; keeping {}",
                dir.display()
            ));
            Ok(dir)
        }
        None => Err(format!(
            "winget finished but no Roblox version directory was found under {}",
            root.display()
        )),
    }
}

/// Removes the stock Roblox install through winget. Leftover version
/// directories that winget missed are cleaned up afterwards.
pub fn uninstall(paths: &Paths) -> Result<(), String> {
    crate::processes::kill_images(&["RobloxPlayerBeta.exe"]);
    if winget_available() {
        winget("uninstall")?;
    } else {
        logger::warn("winget is unavailable; removing the leftover Roblox version directories only");
    }
    let root = versions_root(paths);
    if root.exists() {
        let _ = std::fs::remove_dir_all(&root);
    }
    logger::info("Stock Roblox install removed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_root_reports_no_install() {
        let tmp = std::env::temp_dir().join(format!(
            "ruststrap-stock-test-{}",
            std::process::id()
        ));
        let paths = Paths {
            config_root: tmp.join("cfg"),
            data_root: tmp.join("data"),
            downloads: tmp.join("data/Dl"),
            versions: tmp.join("data/V"),
            modifications: tmp.join("cfg/M"),
            logs: tmp.join("state/L"),
            roblox: tmp.join("local/Roblox"),
        };
        assert!(installed_dir(&paths, None).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn versions_root_is_under_the_stock_location() {
        let tmp = std::env::temp_dir().join("ruststrap-stock-root-test");
        let paths = Paths {
            config_root: tmp.clone(),
            data_root: tmp.clone(),
            downloads: tmp.clone(),
            versions: tmp.clone(),
            modifications: tmp.clone(),
            logs: tmp.clone(),
            roblox: tmp.join("local/Roblox"),
        };
        assert_eq!(versions_root(&paths), tmp.join("local/Roblox/Versions"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
