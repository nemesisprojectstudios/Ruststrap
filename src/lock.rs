// SPDX-FileCopyrightText: 2026 Ruststrap Contributors
//
// SPDX-License-Identifier: MPL-2.0

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::logger;

/// A very small complementary process lock implemented with O_EXCL file creation.
/// No cross-platform libraries needed; a crashed stale lock is reclaimed after
/// `STALE_AFTER`.
pub struct FileLock {
    path: Option<PathBuf>,
}

const STALE_AFTER: Duration = Duration::from_secs(30 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

impl FileLock {
    /// Returns None while another process holds the lock with the same name.
    pub fn try_acquire(dir: &Path, name: &str) -> Option<FileLock> {
        let path = dir.join(format!("{name}.lock"));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(_) => Some(FileLock { path: Some(path) }),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => None,
            Err(err) => {
                logger::error(&format!("Failed to create lock {name}: {err}"));
                // proceed unlocked rather than wedging the bootstrapper
                Some(FileLock { path: None })
            }
        }
    }

    /// Blocks until the lock is free (reclaiming stale locks), polling.
    pub fn acquire(dir: &Path, name: &str) -> FileLock {
        loop {
            if let Some(lock) = FileLock::try_acquire(dir, name) {
                return lock;
            }
            logger::info(&format!("{name} instance exists, waiting..."));
            if let Ok(meta) = std::fs::metadata(dir.join(format!("{name}.lock"))) {
                if meta
                    .modified()
                    .map(|modified| SystemTime::now().duration_since(modified).unwrap_or_default() > STALE_AFTER)
                    .unwrap_or(false)
                {
                    logger::warn(&format!("Reclaiming stale {name} lock"));
                    let _ = std::fs::remove_file(dir.join(format!("{name}.lock")));
                    continue;
                }
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}
