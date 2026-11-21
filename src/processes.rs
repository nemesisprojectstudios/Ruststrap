// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::process::{Command, Stdio};

use crate::logger;

fn run_quiet(cmd: &mut Command) -> bool {
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    match cmd.status() {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

/// Forcefully kills processes by image name (Roblox clients and its crash handler
/// must be gone before packages can be overwritten on Windows).
pub fn kill_images(images: &[&str]) {
    for image in images {
        if cfg!(windows) {
            if !run_quiet(Command::new("taskkill").args(["/F", "/IM", image])) {
                logger::info(&format!("{image} was not running"));
            }
        } else if !run_quiet(Command::new("pkill").args(["-x", image])) {
            logger::info(&format!("{image} was not running"));
        }
    }
}

pub fn roblox_player_image() -> &'static str {
    if cfg!(windows) {
        "RobloxPlayerBeta.exe"
    } else {
        "sober"
    }
}

pub fn roblox_crash_handler_image() -> &'static str {
    "RobloxCrashHandler.exe"
}
