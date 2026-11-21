// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::logger;
use crate::paths::Paths;

/// Deletes logs and caches older than `settings.cleaner_days` (0 = never clean).
/// Runs after Roblox exits, like Froststrap's Cleaner.
pub fn clean(paths: &Paths, settings: &crate::state::Settings) {
    if settings.cleaner_days == 0 {
        return;
    }
    clean_now(paths, settings.cleaner_days);
}

/// A manual clean from the UI ignores the 0-day switch and uses 7 days by default.
pub fn clean_now(paths: &Paths, days: u32) {
    let roblox_logs = if cfg!(windows) {
        paths.roblox.join("logs")
    } else {
        paths.roblox.join("data/sober/sober_logs")
    };
    let roblox_cache = if cfg!(windows) {
        std::env::temp_dir().join("Roblox")
    } else {
        paths.roblox.join("cache")
    };

    let targets: [(&str, PathBuf); 4] = [
        ("RuststrapLogs", paths.logs.clone()),
        ("Downloads", paths.downloads.clone()),
        ("RobloxLogs", roblox_logs),
        ("RobloxCache", roblox_cache),
    ];

    let cutoff = SystemTime::now() - Duration::from_secs(days as u64 * 86400);
    for (name, dir) in targets {
        if !dir.is_dir() {
            continue;
        }
        let deleted = clean_dir(&dir, cutoff, 2000);
        logger::info(&format!("Cleaner removed {deleted} old file(s) from {name}"));
    }
}

fn clean_dir(dir: &Path, cutoff: SystemTime, limit: usize) -> usize {
    let mut deleted = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        if deleted >= limit {
            return deleted;
        }
        let path = entry.path();
        if path.is_dir() {
            deleted += clean_dir(&path, cutoff, limit - deleted);
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map(|modified| modified < cutoff)
            .unwrap_or(false);
        if old && std::fs::remove_file(&path).is_ok() {
            deleted += 1;
        }
    }
    deleted
}
