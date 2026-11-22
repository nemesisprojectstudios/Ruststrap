// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::time::{Duration, Instant};

use crate::logger;

/// How long the child's process handle may be held before switching to a
/// name-based poll. Roblox's Hyperion anti-tamper terminates the client when
/// a foreign handle to its process stays open for over a minute (Froststrap
/// hit the same wall), so the handle is dropped well before that.
const HANDLE_HOLD_LIMIT: Duration = Duration::from_secs(30);

/// Waits until the game process exits, mirroring Froststrap's watcher contract:
/// keep the bootstrapper alive while the client runs, hand over to cleanup after.
/// On Windows the handle is released after a grace period and the watch continues
/// by image name so Hyperion never sees a long-lived foreign handle.
pub fn wait_for_exit(mut child: std::process::Child, image: &str) {
    let deadline = Instant::now() + HANDLE_HOLD_LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                logger::info(&format!("Roblox exited with {status}"));
                return;
            }
            Ok(None) => {
                if cfg!(windows) && Instant::now() >= deadline {
                    drop(child);
                    watch_by_name(image);
                    return;
                }
                std::thread::sleep(Duration::from_millis(1000));
            }
            Err(e) => {
                logger::error(&format!("Failed to poll Roblox process: {e}"));
                logger::info("Assuming Roblox exited");
                return;
            }
        }
    }
}

fn watch_by_name(image: &str) {
    logger::info("Watching Roblox by process name (handle released)");
    loop {
        if !crate::processes::image_running(image) {
            logger::info("Roblox exited");
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
