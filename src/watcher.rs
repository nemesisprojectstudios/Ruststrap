// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::time::Duration;

use crate::logger;

/// Waits until the game process exits, mirroring Froststrap's watcher contract:
/// keep the bootstrapper alive while the client runs, hand over to cleanup after.
pub fn wait_for_exit(child: &mut std::process::Child) {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                logger::info(&format!("Roblox exited with {status}"));
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(1000)),
            Err(e) => {
                logger::error(&format!("Failed to poll Roblox process: {e}"));
                logger::info("Assuming Roblox exited");
                return;
            }
        }
    }
}
