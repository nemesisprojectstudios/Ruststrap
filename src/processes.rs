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

/// Cheap existence check by image name; used where holding a process
/// handle is not an option (see watcher::wait_for_exit).
pub fn image_running(image: &str) -> bool {
    if cfg!(windows) {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("IMAGENAME eq {image}"), "/NH"])
            .output();
        match output {
            Ok(out) => String::from_utf8_lossy(&out.stdout)
                .to_ascii_lowercase()
                .contains(&image.to_ascii_lowercase()),
            Err(_) => false,
        }
    } else {
        // pkill exits 0 when a process matched
        run_quiet(Command::new("pkill").args(["-0", "-x", image]))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_running_reports_absent_process() {
        assert!(!image_running("ruststrap-not-a-real-image-name-42"));
    }
}
